package main

import (
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"golang.org/x/net/html"
)

const statsFileName = "stats.json"

var blockedDomains = map[string]bool{
	"doubleclick.net":        true,
	"googlesyndication.com":  true,
	"google-analytics.com":   true,
	"googletagmanager.com":   true,
	"gemius.pl":              true,
	"ads.pubmatic.com":       true,
}

var trackingParams = map[string]bool{
	"utm_source":   true,
	"utm_medium":   true,
	"utm_campaign": true,
	"utm_term":     true,
	"utm_content":  true,
	"utm_id":       true,
	"src":          true,
	"tpcc":         true,
	"fbclid":       true,
	"gclid":        true,
}

type Stats struct {
	mu             sync.Mutex
	PagesFetched   int   `json:"pages_fetched"`
	TotalRawBytes  int64 `json:"total_raw_bytes"`
	TotalCleanByte int64 `json:"total_clean_bytes"`
	TotalSavedByte int64 `json:"total_saved_bytes"`
	SkippedImages  int   `json:"skipped_images"`
	SkippedVideos  int   `json:"skipped_videos"`
	SkippedLinks   int   `json:"skipped_links"`
	BlockedDomains int   `json:"blocked_domains"`
}

type FetchResult struct {
	URL            string  `json:"url"`
	RawBytes       int     `json:"raw_bytes"`
	CleanBytes     int     `json:"clean_bytes"`
	SavedBytes     int     `json:"saved_bytes"`
	SavedPct       float64 `json:"saved_pct"`
	SkippedImages  int     `json:"skipped_images"`
	SkippedVideos  int     `json:"skipped_videos"`
	SkippedLinks   int     `json:"skipped_links"`
	BlockedDomains int     `json:"blocked_domains"`
	TextView       string  `json:"text_view"`
}

type ExtractMetrics struct {
	TextView       string
	SkippedImages  int
	SkippedVideos  int
	SkippedLinks   int
	BlockedDomains int
}

var globalStats = &Stats{}

func statsFilePath() string {
	if envPath := os.Getenv("GRST_STATS_FILE"); envPath != "" {
		return envPath
	}
	return statsFileName
}

func loadStats() {
	globalStats.mu.Lock()
	defer globalStats.mu.Unlock()

	data, err := os.ReadFile(statsFilePath())
	if err != nil {
		return
	}
	_ = json.Unmarshal(data, globalStats)
}

func saveStatsLocked() {
	payload, err := json.MarshalIndent(globalStats, "", "  ")
	if err != nil {
		return
	}
	targetPath := statsFilePath()
	tmpFile := targetPath + ".tmp"
	if err := os.WriteFile(tmpFile, payload, 0644); err != nil {
		log.Printf("Błąd zapisu %s: %v", tmpFile, err)
		return
	}
	_ = os.Rename(tmpFile, filepath.Clean(targetPath))
}

func recordBlockedDomainOnly() {
	globalStats.mu.Lock()
	defer globalStats.mu.Unlock()
	globalStats.BlockedDomains++
	saveStatsLocked()
}

func recordStatsAndSave(rawBytes, cleanBytes, savedBytes, imgs, vids, links, blocked int) {
	globalStats.mu.Lock()
	defer globalStats.mu.Unlock()

	globalStats.PagesFetched++
	globalStats.TotalRawBytes += int64(rawBytes)
	globalStats.TotalCleanByte += int64(cleanBytes)
	globalStats.TotalSavedByte += int64(savedBytes)
	globalStats.SkippedImages += imgs
	globalStats.SkippedVideos += vids
	globalStats.SkippedLinks += links
	globalStats.BlockedDomains += blocked

	saveStatsLocked()
}

func isHostBlocked(rawURL string) bool {
	u, err := url.Parse(rawURL)
	if err != nil {
		return false
	}
	host := strings.ToLower(u.Hostname())
	for blocked := range blockedDomains {
		if host == blocked || strings.HasSuffix(host, "."+blocked) {
			return true
		}
	}
	return false
}

func cleanTrackingURL(rawURL string) string {
	u, err := url.Parse(rawURL)
	if err != nil {
		return rawURL
	}
	q := u.Query()
	for param := range q {
		if trackingParams[strings.ToLower(param)] {
			q.Del(param)
		}
	}
	u.RawQuery = q.Encode()
	return u.String()
}

func collapseBlankLines(input string) string {
	lines := strings.Split(input, "\n")
	var out []string
	emptyStreak := 0

	for _, line := range lines {
		trimmedLine := strings.TrimSpace(line)
		// Zagęszczamy wielokrotne spacje wewnątrz linii
		cleanedLine := strings.Join(strings.Fields(trimmedLine), " ")
		if cleanedLine == "" {
			emptyStreak++
			if emptyStreak <= 1 {
				out = append(out, "")
			}
			continue
		}
		emptyStreak = 0
		out = append(out, cleanedLine)
	}

	return strings.TrimSpace(strings.Join(out, "\n"))
}

func isSkippableHref(rawHref string) bool {
	trimmed := strings.TrimSpace(rawHref)
	if trimmed == "" || trimmed == "#" {
		return true
	}
	lower := strings.ToLower(trimmed)
	return strings.HasPrefix(lower, "javascript:") || strings.HasPrefix(lower, "mailto:")
}

func extractLynxView(r io.Reader, baseURL string) ExtractMetrics {
	doc, err := html.Parse(r)
	if err != nil {
		return ExtractMetrics{TextView: "Błąd parsowania HTML: " + err.Error()}
	}

	var out strings.Builder
	linkCounter := 1
	var metrics ExtractMetrics

	resolveURL := func(rawHref string) string {
		trimmed := strings.TrimSpace(rawHref)
		if trimmed == "" {
			return ""
		}
		base, err := url.Parse(baseURL)
		if err != nil {
			return trimmed
		}
		ref, err := url.Parse(trimmed)
		if err != nil {
			return trimmed
		}
		return base.ResolveReference(ref).String()
	}

	var walk func(*html.Node)
	walk = func(n *html.Node) {
		if n.Type == html.ElementNode {
			switch n.Data {
			case "script", "style", "noscript", "svg":
				return
			case "h1", "h2", "h3", "p", "li", "br":
				out.WriteString("\n")
			case "img":
				src := resolveURL(getAttr(n, "src"))
				if isSkippableHref(src) {
					metrics.SkippedImages++
					return
				}
				if isHostBlocked(src) {
					metrics.BlockedDomains++
					return
				}
				src = cleanTrackingURL(src)
				alt := strings.Join(strings.Fields(getAttr(n, "alt")), " ")
				if alt == "" {
					alt = "obraz"
				}
				metrics.SkippedImages++
				fmt.Fprintf(&out, "\n[OBRAZ: %s -> %s]\n", alt, src)
				return
			case "video":
				src := resolveURL(getAttr(n, "src"))
				if isSkippableHref(src) {
					metrics.SkippedVideos++
					return
				}
				if isHostBlocked(src) {
					metrics.BlockedDomains++
					return
				}
				src = cleanTrackingURL(src)
				metrics.SkippedVideos++
				fmt.Fprintf(&out, "\n[WIDEO -> %s]\n", src)
				return
			case "a":
				rawHref := getAttr(n, "href")
				label := strings.Join(strings.Fields(collectText(n)), " ")
				if isSkippableHref(rawHref) {
					metrics.SkippedLinks++
					if label != "" {
						out.WriteString(" " + label + " ")
					}
					return
				}

				resolved := resolveURL(rawHref)
				if isHostBlocked(resolved) {
					metrics.BlockedDomains++
					return
				}
				resolved = cleanTrackingURL(resolved)

				if label == "" {
					metrics.SkippedLinks++
					return
				}
				fmt.Fprintf(&out, " [%d: %s -> %s] ", linkCounter, label, resolved)
				linkCounter++
				return
			}
		}

		if n.Type == html.TextNode {
			txt := strings.Join(strings.Fields(n.Data), " ")
			if txt != "" {
				out.WriteString(txt + " ")
			}
		}

		for c := n.FirstChild; c != nil; c = c.NextSibling {
			walk(c)
		}
	}

	walk(doc)
	metrics.TextView = collapseBlankLines(out.String())
	return metrics
}

func getAttr(n *html.Node, key string) string {
	for _, a := range n.Attr {
		if a.Key == key {
			return a.Val
		}
	}
	return ""
}

func collectText(n *html.Node) string {
	var b strings.Builder
	var f func(*html.Node)
	f = func(node *html.Node) {
		if node.Type == html.TextNode {
			b.WriteString(node.Data)
		}
		for c := node.FirstChild; c != nil; c = c.NextSibling {
			f(c)
		}
	}
	f(n)
	return b.String()
}

func fetchHandler(w http.ResponseWriter, r *http.Request) {
	target := r.URL.Query().Get("url")
	if target == "" {
		http.Error(w, "Podaj parametr ?url=", http.StatusBadRequest)
		return
	}

	if isHostBlocked(target) {
		recordBlockedDomainOnly()
		http.Error(w, "Domena zablokowana przez GRST", http.StatusForbidden)
		return
	}

	client := &http.Client{Timeout: 10 * time.Second}
	req, err := http.NewRequest(http.MethodGet, target, nil)
	if err != nil {
		http.Error(w, "Błędny URL: "+err.Error(), http.StatusBadRequest)
		return
	}
	req.Header.Set("User-Agent", "GRST-LynxLite/0.1")

	resp, err := client.Do(req)
	if err != nil {
		http.Error(w, "Błąd pobierania: "+err.Error(), http.StatusBadGateway)
		return
	}
	defer resp.Body.Close()

	rawBody, err := io.ReadAll(resp.Body)
	if err != nil {
		http.Error(w, "Błąd odczytu ciała: "+err.Error(), http.StatusInternalServerError)
		return
	}

	m := extractLynxView(strings.NewReader(string(rawBody)), target)
	rawBytes := len(rawBody)
	cleanBytes := len(m.TextView)
	savedBytes := rawBytes - cleanBytes
	if savedBytes < 0 {
		savedBytes = 0
	}

	recordStatsAndSave(rawBytes, cleanBytes, savedBytes, m.SkippedImages, m.SkippedVideos, m.SkippedLinks, m.BlockedDomains)
	log.Printf("URL=%s | raw=%dB | clean=%dB | saved=%dB | img=%d | vid=%d | skipLinks=%d | blocked=%d",
		target, rawBytes, cleanBytes, savedBytes, m.SkippedImages, m.SkippedVideos, m.SkippedLinks, m.BlockedDomains)

	if r.URL.Query().Get("raw") == "1" {
		w.Header().Set("Content-Type", "text/plain; charset=utf-8")
		_, _ = w.Write([]byte(m.TextView + "\n"))
		return
	}

	var savedPct float64
	if rawBytes > 0 {
		savedPct = float64(savedBytes) / float64(rawBytes) * 100.0
	}

	res := FetchResult{
		URL:            target,
		RawBytes:       rawBytes,
		CleanBytes:     cleanBytes,
		SavedBytes:     savedBytes,
		SavedPct:       savedPct,
		SkippedImages:  m.SkippedImages,
		SkippedVideos:  m.SkippedVideos,
		SkippedLinks:   m.SkippedLinks,
		BlockedDomains: m.BlockedDomains,
		TextView:       m.TextView,
	}

	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	_ = json.NewEncoder(w).Encode(res)
}

func statsHandler(w http.ResponseWriter, r *http.Request) {
	globalStats.mu.Lock()
	defer globalStats.mu.Unlock()

	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	_ = json.NewEncoder(w).Encode(globalStats)
}

func main() {
	loadStats()
	http.HandleFunc("/fetch", fetchHandler)
	http.HandleFunc("/stats", statsHandler)
	fmt.Println("GRST działa na http://localhost:8080 (endpointy: /fetch oraz /stats)")
	log.Fatal(http.ListenAndServe(":8080", nil))
}
