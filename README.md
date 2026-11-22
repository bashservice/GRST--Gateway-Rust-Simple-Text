# 🖥️ GRST-MC (`grst-cli`)

Terminalowa przeglądarka internetowa (TUI) napisana w języku **Rust**, inspirowana klasycznym interfejsem **Midnight Commander**. Aplikacja współpracuje z wbudowanym lokalnym serwerem proxy (`grst-proxy`), kompresując treść stron i renderując multimedia w stylu retro (czarno-biała grafika Braille'a z ditheringiem Floyda-Steinberga oraz ekstrakcja audio do OGG).

---

## ✨ Główne funkcje

* **Interfejs TUI w stylu Midnight Commander:** Dwa aktywne panele (treść strony oraz lista linków), pasek statusu, historia przeglądania i dolny pasek skrótów — zaimplementowane bez ciężkich zewnętrznych bibliotek TUI (czyste kody ANSI + `stty`).
* **Wbudowane lokalne Proxy (RAII):** Binarka zawiera osadzony serwer `grst-proxy`, który automatycznie uruchamia się w tle na `127.0.0.1:8080` przy starcie programu i jest bezpiecznie zamykany przy wyjściu (`ProxyGuard`).
* **Inteligentne formatowanie treści:**
  * Parsowanie linków i mediów w składni `[1:Opis->https://...]` oraz `[OBRAZ:Opis->URL]`, `[VIDEO:Opis->URL]`.
  * Automatyczne wykrywanie pionowych menu nawigacyjnych i spłaszczanie ich do kompaktowego układu poziomego (`│`).
* **Grafika Retro 1980 B&W (`img_retro`):**
  * Obsługa obrazów HTTP oraz `data:image/...;base64` (limit do **1 MB**).
  * Wyostrzanie krawędzi, korekcja gamma/kontrastu, dithering **Floyda-Steinberga** oraz rendering w wysokiej rozdzielczości przy użyciu znaków **Unicode Braille** (`2×4` subpiksele na znak).
  * Interaktywny podgląd modalny z płynną zmianą skali (`-`, `+`, `z`).
* **Obsługa wideo i audio OGG (`video_ogg`):**
  * Pobieranie klipów do **5 MB** i automatyczna ekstrakcja ścieżki dźwiękowej do `/tmp/grst_audio.ogg` za pomocą `ffmpeg` (`libvorbis`) z mechanizmem fallback.
* **Statystyki oszczędności transferu:** Podgląd danych z endpointu `/stats` bezpośrednio w przeglądarce.


[demo-grst.webm](https://github.com/user-attachments/assets/e7412cf4-0d02-4058-b0d3-6db76aec7714)


## 🏗️ Architektura i schemat działania

```mermaid
flowchart TD
    User([Użytkownik w Terminalu]) <-->|Klawiatura / ANSI TUI| CLI

    subgraph GRST_CLI ["grst-cli (Rust TUI)"]
        CLI[main.rs: Pętla zdarzeń & Renderer MC]
        Parser[Parser dokumentu & Spłaszczanie menu]
        Forms[forms.rs: Stan pól formularzy]
        ImgRetro[img_retro.rs: Floyd-Steinberg + Braille]
        VidOgg[video_ogg.rs: Ekstrakcja audio .ogg]
        
        CLI --> Parser
        CLI --> Forms
        CLI --> ImgRetro
        CLI --> VidOgg
    end

    subgraph Local_System ["Środowisko lokalne (/tmp)"]
        Proxy["grst-proxy (127.0.0.1:8080)"]
        FFmpeg[ffmpeg / libvorbis]
        OggFile[(/tmp/grst_audio.ogg)]
    end

    Internet((Internet / WWW))

    CLI -.->|Wypakowuje i zarządza procesem| Proxy
    CLI <-->|HTTP GET /fetch & /stats| Proxy
    Proxy <-->|Pobieranie i uproszczenie HTML/Mediów| Internet
    VidOgg -->|Transkodowanie < 5MB| FFmpeg
    FFmpeg --> OggFile
