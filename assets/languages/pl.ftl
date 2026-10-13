app-title = Fennec
search-hint = Szukaj
col-name = Nazwa
col-path = Ścieżka
col-size = Rozmiar
col-modified = Data modyfikacji
status-objects = { $count ->
    [one] { $count } obiekt
    [few] { $count } obiekty
    [many] { $count } obiektów
   *[other] { $count } obiektów
}
status-selected = Zaznaczono: { $count }
status-elapsed = { $ms } ms
index-starting = Uruchamianie…
index-building = Indeksowanie { $volume }: przetworzono { $done }
index-meta = Odczytywanie rozmiarów i dat: { $percent }%
index-ready = Zaindeksowano wpisów: { $count } w { $seconds } s
index-error = Błąd indeksu: { $message }
index-db-rebuild = Nie można odczytać bazy danych indeksu; trwa jej odbudowa ({ $message }).
index-volume-rebuild = Dziennik zmian woluminu { $volume } uległ zmianie; wolumin jest indeksowany ponownie.
index-volume-added = Dodano { $volume } do indeksu.
index-watch-error = Monitorowanie { $volume } zatrzymane: { $message }
index-folder-fallback = Brak dostępu do dziennika NTFS woluminu { $volume } (uruchom jako administrator lub zainstaluj usługę); jest on indeksowany jako folder.
empty-results = Brak wyników
empty-indexing = Tworzenie indeksu…
action-open = Otwórz
action-open-path = Otwórz ścieżkę
action-copy-path = Kopiuj pełną ścieżkę
action-copy-name = Kopiuj nazwę
tray-show = Pokaż Fennec
tray-exit = Zakończ
msg-copied = Skopiowano do schowka
msg-open-failed = Nie można otworzyć { $path }: { $error }
diag-unsupported = Jeszcze nieobsługiwane: { $name }:
diag-invalid = Nieprawidłowa wartość { $name }: { $value }
diag-pattern = Nieprawidłowy wzorzec: { $message }
diag-quote = Niezamknięty cudzysłów
diag-group = Niezamknięty nawias <
msg-restart-required = Uruchom ponownie Fennec, aby zmienić indeksowane foldery i listy plików.
cmd-focus-search = Przejdź do wyszukiwania
cmd-refresh = Odśwież
cmd-open = Otwórz
cmd-open-path = Otwórz ścieżkę
cmd-open-with = Otwórz za pomocą…
cmd-run-as-admin = Uruchom jako administrator
cmd-properties = Właściwości
cmd-context-menu = Menu kontekstowe
cmd-copy = Kopiuj
cmd-cut = Wytnij
cmd-paste = Wklej
cmd-copy-full-path = Kopiuj pełną ścieżkę
cmd-copy-full-path-quoted = Kopiuj pełną ścieżkę (w cudzysłowie)
cmd-copy-name = Kopiuj nazwę
cmd-copy-path = Kopiuj ścieżkę
cmd-copy-as-tsv = Kopiuj jako TSV
cmd-copy-as-csv = Kopiuj jako CSV
cmd-copy-as-efu = Kopiuj jako listę plików
cmd-delete = Usuń
cmd-delete-permanently = Usuń trwale
cmd-rename = Zmień nazwę
cmd-new-folder = Nowy folder
cmd-explore-in-fennec = Przeglądaj w Fennec
cmd-omit = Pomiń wyniki
cmd-set-run-count = Ustaw liczbę uruchomień…
cmd-select-all = Zaznacz wszystko
cmd-invert-selection = Odwróć zaznaczenie
cmd-toggle-selection = Przełącz zaznaczenie
cmd-move-down = W dół
cmd-move-up = W górę
cmd-extend-down = Rozszerz zaznaczenie w dół
cmd-extend-up = Rozszerz zaznaczenie w górę
cmd-focus-down = Fokus w dół
cmd-focus-up = Fokus w górę
cmd-page-down = Strona w dół
cmd-page-up = Strona w górę
cmd-first = Pierwszy wynik
cmd-last = Ostatni wynik
cmd-match-case = Uwzględniaj wielkość liter
cmd-match-whole-word = Tylko całe wyrazy
cmd-match-path = Uwzględniaj ścieżkę
cmd-match-diacritics = Uwzględniaj znaki diakrytyczne
cmd-enable-regex = Wyrażenia regularne
cmd-advanced-search = Wyszukiwanie zaawansowane…
cmd-add-bookmark = Dodaj do zakładek…
cmd-organize-bookmarks = Organizuj zakładki…
cmd-organize-filters = Organizuj filtry…
cmd-add-filter = Dodaj filtr…
cmd-filter-everything = Wszystko
cmd-options = Opcje…
cmd-export = Eksportuj…
cmd-toggle-menu-bar = Pasek menu
cmd-toggle-status-bar = Pasek stanu
cmd-toggle-filter-bar = Pasek filtrów
cmd-theme-light = Jasny
cmd-theme-dark = Ciemny
cmd-theme-system = Zgodnie z systemem
cmd-zoom-in = Powiększ
cmd-zoom-out = Pomniejsz
cmd-zoom-reset = Resetuj powiększenie
cmd-search-history = Historia wyszukiwania
cmd-clear-history = Wyczyść historię wyszukiwania
cmd-install-service = Zainstaluj usługę Fennec
cmd-uninstall-service = Odinstaluj usługę Fennec
cmd-save-database = Zapisz indeks teraz
cmd-pause-updates = Wstrzymaj aktualizacje indeksu
cmd-help = Składnia wyszukiwania
cmd-about = Informacje o programie Fennec
cmd-hide-window = Ukryj okno
cmd-exit = Zakończ
cmd-filter-1 = Filtr 1
cmd-filter-2 = Filtr 2
cmd-filter-3 = Filtr 3
cmd-filter-4 = Filtr 4
cmd-filter-5 = Filtr 5
cmd-filter-6 = Filtr 6
cmd-filter-7 = Filtr 7
cmd-filter-8 = Filtr 8
cmd-filter-9 = Filtr 9
col-ext = Rozszerzenie
col-type = Typ
col-dm = Data modyfikacji
col-dc = Data utworzenia
col-da = Data dostępu
col-attributes = Atrybuty
col-runcount = Liczba uruchomień
col-dr = Data uruchomienia
col-rc = Data ostatniej zmiany
col-fullpath = Pełna ścieżka
col-filelist = Nazwa pliku listy
col-indexsource = Źródło indeksu
col-volume = Wolumin
col-depth = Głębokość
col-namelength = Długość nazwy
col-frn = Numer odwołania do pliku
options-page-general = Ogólne
options-desc-general = Język, pojedyncze wystąpienie, zasobnik i skrót klawiszowy.
options-page-ui = Interfejs
options-desc-ui = Motyw, powiększenie, wysokość wierszy i widoczne paski.
options-page-search = Wyszukiwanie
options-desc-search = Domyślny sposób dopasowywania wyszukiwań.
options-page-results = Wyniki
options-desc-results = Wyróżnianie, kolejność folderów i otwieranie.
options-page-view = Widok
options-desc-view = Kolumny, kolejność sortowania i formaty.
options-page-fonts_colors = Czcionki i kolory
options-desc-fonts_colors =  
options-page-history = Historia
options-desc-history = Historia wyszukiwania i uruchomień.
options-page-keyboard = Klawiatura
options-desc-keyboard =  
options-page-context_menu = Menu kontekstowe
options-desc-context_menu = Integracja z Eksploratorem plików jest instalowana razem z pozycjami menu kontekstowego (fennec -install-shell, M4).
options-page-open_commands = Polecenia otwierania
options-desc-open_commands = Własne polecenia otwierania: Etykieta|program argumenty, %1 = pełna ścieżka.
options-page-index = Indeksy
options-desc-index = Co jest indeksowane i jak jest zapisywana baza danych.
options-page-ntfs = NTFS / ReFS
options-desc-ntfs = Woluminy NTFS/ReFS i ich dzienniki zmian.
options-page-folders = Foldery
options-desc-folders = Indeksy folderów (FAT, sieć, dowolny folder).
options-page-filelists = Listy plików
options-desc-filelists = Zamontowane listy plików EFU.
options-page-exclude = Wykluczenia
options-desc-exclude = Pliki ukryte i systemowe oraz wykluczone foldery.
options-page-properties = Właściwości
options-desc-properties = Indeksowanie właściwości (szerokość, długość, autor…).
options-page-content = Zawartość
options-desc-content = Indeksowanie zawartości.
options-page-semantic = Wyszukiwanie semantyczne
options-desc-semantic = Lokalne wyszukiwanie semantyczne z EmbeddingGemma-2. Pobierz model i sprawdź jego stan w menu Narzędzia → Wyszukiwanie semantyczne…
options-page-index_journal = Dziennik indeksu
options-desc-index_journal = Dziennik indeksu (data ostatniej zmiany).
options-page-server = Serwery
options-desc-server = Serwer IPC dla fes.exe i SDK, okno IPC zgodne z Everything (es.exe, programy uruchamiające) oraz serwer HTTP.
options-page-advanced = Zaawansowane
options-desc-advanced =  
keyctx-search = Pole wyszukiwania
keyctx-results = Lista wyników
keyctx-global = Globalne
filter-audio = Audio
filter-compressed = Archiwa
filter-document = Dokumenty
filter-executable = Pliki wykonywalne
filter-folder = Foldery
filter-picture = Obrazy
filter-video = Wideo
filter-everything = Wszystko
menu-file = Plik
menu-edit = Edycja
menu-view = Widok
menu-search = Wyszukiwanie
menu-bookmarks = Zakładki
menu-tools = Narzędzia
menu-help = Pomoc
menu-copy-as = Kopiuj jako
menu-theme = Motyw
menu-zoom = Powiększenie
menu-columns = Kolumny
menu-reset-columns = Resetuj kolumny
tray-options = Opcje…
tray-pause = Wstrzymaj/wznów aktualizacje indeksu
status-paused = aktualizacje wstrzymane
type-folder = Folder plików
type-file = Plik
source-folder = Folder
source-filelist = Lista plików
time-just-now = przed chwilą
new-folder-name = Nowy folder
dialog-ok = OK
dialog-cancel = Anuluj
dialog-close = Zamknij
dialog-add = Dodaj…
dialog-edit = Edytuj…
dialog-delete = Usuń
dialog-import = Importuj z Everything
dialog-create = Utwórz
dialog-search = Szukaj
dialog-save = Zapisz
dialog-edit-filter = Filtr
dialog-run-count = Liczba uruchomień:
dialog-export-help = Eksportuj wyniki do pliku; format zależy od rozszerzenia: .efu (lista plików), .csv / .tsv / .json (widoczne kolumny), .m3u / .m3u8, każde inne — jedna ścieżka w wierszu.
field-name = Nazwa
field-search = Wyszukiwanie
field-filter = Filtr
field-sort = Sortowanie
field-folder = Folder
field-macro = Makro
builder-all-words = Wszystkie te wyrazy
builder-exact-phrase = Dokładnie to wyrażenie
builder-any-words = Dowolny z tych wyrazów
builder-none-words = Żaden z tych wyrazów
builder-match-case = Uwzględniaj wielkość liter
builder-whole-word = Tylko całe wyrazy
builder-folder = W folderze
builder-subfolders = Uwzględnij podfoldery
builder-extensions = Rozszerzenia
builder-kind = Szukaj
builder-kind-all = Pliki i foldery
builder-kind-files = Pliki
builder-kind-folders = Foldery
builder-size = Rozmiar
builder-date = Data modyfikacji
builder-attributes = Atrybuty
builder-hidden = Ukryty
builder-system = Systemowy
builder-readonly = Tylko do odczytu
builder-preview = Zapytanie:
options-title = Opcje programu Fennec
options-ok = OK
options-cancel = Anuluj
options-apply = Zastosuj
options-reset = Resetuj
options-reset-keys = Przywróć domyślne skróty
options-show-containing = Pokaż ustawienia zawierające:
options-empty-page = Na razie nie ma tu nic do skonfigurowania.
options-command = Polecenie
options-press-key = Naciśnij klawisz…
options-filter-hint = Filtruj polecenia
options-keyboard-help = Kliknij skrót, aby go zmienić; ✕ go usuwa.
msg-copied-files = { $count ->
    [one] Skopiowano do schowka { $count } element
    [few] Skopiowano do schowka { $count } elementy
    [many] Skopiowano do schowka { $count } elementów
   *[other] Skopiowano do schowka { $count } elementów
}
msg-pasted = Wklejono
msg-op-failed = Operacja nie powiodła się: { $error }
msg-deleted = { $count ->
    [one] Usunięto { $count } element
    [few] Usunięto { $count } elementy
    [many] Usunięto { $count } elementów
   *[other] Usunięto { $count } elementów
}
msg-renamed = Zmieniono nazwę na { $name }
msg-created = Utworzono { $name }
msg-db-saved = Zapisano indeks
msg-service-requested = Uruchomiono polecenie usługi
msg-history-cleared = Wyczyszczono historię wyszukiwania
msg-setting-changed = { $key } = { $value }
msg-unknown-command = Nieznane polecenie: /{ $command }
msg-bad-bindings = Zignorowano nieprawidłowe skróty klawiszowe: { $bindings }
msg-updates-paused = Wstrzymano aktualizacje indeksu
msg-updates-resumed = Wznowiono aktualizacje indeksu
msg-imported = Zaimportowano z { $file }: { $count }
msg-import-missing = Nie znaleziono { $file } (przeszukano foldery danych Fennec i Everything)
msg-exported = { $count ->
    [one] Wyeksportowano { $count } wynik do { $path }
    [few] Wyeksportowano { $count } wyniki do { $path }
    [many] Wyeksportowano { $count } wyników do { $path }
   *[other] Wyeksportowano { $count } wyników do { $path }
}
about-text =
    Znajdź wszystko w mgnieniu oka.
    Wyszukiwarka nazw plików zgodna ze składnią wyszukiwania Everything 1.5.
time-minutes-ago = { $n } min temu
time-hours-ago = { $n ->
    [one] { $n } godzinę temu
    [few] { $n } godziny temu
    [many] { $n } godzin temu
   *[other] { $n } godzin temu
}
time-days-ago = { $n ->
    [one] { $n } dzień temu
    [few] { $n } dni temu
    [many] { $n } dni temu
   *[other] { $n } dni temu
}
type-ext-file = Plik { $ext }
dialog-new-folder-in = Nowy folder w { $path }:
help-syntax =
    Składnia wyszukiwania (zgodna z Everything)
        spacja   I        |   LUB        !   NIE       < >   grupowanie       "…"   tekst dosłowny
        {"*"}  ?    symbole wieloznaczne (cała nazwa)   \ w wyrazie dopasowuje pełną ścieżkę   ^abc / abc$ początek / koniec nazwy
    Modyfikatory: case: ww: wfn: path: regex: diacritics: nopath: py: (pinyin) …
    Funkcje: ext:jpg;png  size:>10mb  dm:today  dc:2026  attrib:H  parent:C:\Windows  child:*.mp3  dupe:  empty:  depth:2 …
    Makra: audio: zip: doc: exe: pic: video:
    Polecenia: wpisz /search_delay=100 lub /reindex w polu wyszukiwania i naciśnij Enter.
dialog-confirm-delete = { $count ->
    [one] Trwale usunąć ten element? Tej operacji nie można cofnąć.
    [few] Trwale usunąć te { $count } elementy? Tej operacji nie można cofnąć.
    [many] Trwale usunąć tych { $count } elementów? Tej operacji nie można cofnąć.
   *[other] Trwale usunąć tych { $count } elementów? Tej operacji nie można cofnąć.
}
cmd-index-journal = Dziennik indeksu…
journal-create = Utworzenie
journal-delete = Usunięcie
journal-rename = Zmiana nazwy
journal-move = Przeniesienie
journal-modify = Modyfikacja
journal-filter = Pokaż zmiany zawierające:
journal-count = { $count ->
    [one] { $count } rekord
    [few] { $count } rekordy
    [many] { $count } rekordów
   *[other] { $count } rekordów
}
journal-time = Czas
journal-action = Zmiana
journal-before = Przed
journal-after = Po
cmd-toggle-preview = Okienko podglądu
cmd-view-details = Szczegóły
cmd-view-medium-thumbnails = Średnie miniatury
cmd-view-large-thumbnails = Duże miniatury
cmd-view-extra-large-thumbnails = Bardzo duże miniatury
cmd-next-item = Następny element
cmd-previous-item = Poprzedni element
preview-nothing = Wybierz wynik, aby wyświetlić podgląd.
preview-loading = Ładowanie…
preview-binary = Plik binarny (brak podglądu tekstu)
preview-truncated = Wyświetlany jest tylko pierwszy 1 MB.
preview-selected = { $count ->
    [one] Zaznaczono { $count } element
    [few] Zaznaczono { $count } elementy
    [many] Zaznaczono { $count } elementów
   *[other] Zaznaczono { $count } elementów
}
preview-total-size = Łączny rozmiar: { $size }
preview-contents = Zawartość
preview-children = Pliki: { $files }, foldery: { $folders }
cmd-new-window = Nowe okno
cmd-close-window = Zamknij okno
msg-unknown-filter = Nieznany filtr: { $name }
msg-unknown-bookmark = Nieznana zakładka: { $name }
msg-unknown-sort = Nieznana kolumna sortowania: { $name }
shell-search-fennec = Szukaj w Fennec
shell-explore-in-fennec = Przeglądaj w Fennec
cmd-import-settings = Importuj ustawienia…
cmd-export-settings = Eksportuj ustawienia…
cmd-import-everything-ini = Importuj Everything.ini…
cmd-export-filters = Eksportuj filtry…
cmd-export-bookmarks = Eksportuj zakładki…
cmd-export-search-history = Eksportuj historię wyszukiwania…
menu-import-export = Import / eksport
dialog-transfer-path = Plik:
msg-transfer-done = Gotowe: { $path }
msg-ini-imported = Zaimportowano ustawienia z Everything.ini: { $count }
index-extracting = Właściwości/zawartość: { $done } / { $total }
warn-not-indexed = { $name }: nieindeksowane (włącz indeks w opcjach lub użyj notindexed:{ $name }:)
warn-slow-limit = Wolne wyszukiwanie zatrzymane, odczytane pliki: { $count }; dodaj filtry, aby je zawęzić.
warn-semantic = Wyszukiwanie semantyczne: { $reason }
value-yes = Tak
value-no = Nie
unit-pixels = { $count ->
    [one] { $count } piksel
    [few] { $count } piksele
    [many] { $count } pikseli
   *[other] { $count } pikseli
}
unit-stars = { $count ->
    [one] { $count } gwiazdka
    [few] { $count } gwiazdki
    [many] { $count } gwiazdek
   *[other] { $count } gwiazdek
}
menu-property-columns = Kolumny właściwości
col-dupegroup = Grupa duplikatów
preview-hits = { $count ->
    [one] { $count } dopasowanie
    [few] { $count } dopasowania
    [many] { $count } dopasowań
   *[other] { $count } dopasowań
}
prop-width = Szerokość
prop-height = Wysokość
prop-dimensions = Wymiary
prop-aspect-ratio = Proporcje obrazu
prop-bit-depth = Głębia bitowa
prop-horizontal-resolution = Rozdzielczość pozioma
prop-vertical-resolution = Rozdzielczość pionowa
prop-orientation = Orientacja
prop-date-taken = Data wykonania
prop-camera-maker = Producent aparatu
prop-camera-model = Model aparatu
prop-exposure-time = Czas naświetlania
prop-f-stop = Przysłona
prop-iso-speed = Czułość ISO
prop-focal-length = Ogniskowa
prop-flash = Lampa błyskowa
prop-latitude = Szerokość geograficzna
prop-longitude = Długość geograficzna
prop-altitude = Wysokość n.p.m.
prop-people = Osoby
prop-tags = Tagi
prop-rating = Klasyfikacja
prop-title = Tytuł
prop-subject = Temat
prop-comment = Komentarze
prop-copyright = Prawa autorskie
prop-length = Długość
prop-artist = Wykonawca
prop-album = Album
prop-album-artist = Wykonawca albumu
prop-track = {"#"}
prop-disc = Dysk
prop-genre = Gatunek
prop-year = Rok
prop-composer = Kompozytor
prop-conductor = Dyrygent
prop-publisher = Wydawca
prop-bit-rate = Szybkość transmisji bitów
prop-sample-rate = Częstotliwość próbkowania
prop-channels = Kanały
prop-bits-per-sample = Bity na próbkę
prop-is-variable-bit-rate = Zmienna szybkość transmisji
prop-lyrics = Tekst utworu
prop-mood = Nastrój
prop-bpm = Uderzenia na minutę
prop-initial-key = Tonacja początkowa
prop-encoded-by = Zakodowane przez
prop-frame-width = Szerokość klatki
prop-frame-height = Wysokość klatki
prop-frame-rate = Szybkość klatek
prop-video-bitrate = Szybkość transmisji danych
prop-total-bitrate = Całkowita szybkość transmisji
prop-compression = Kompresja wideo
prop-video-codec = Kodek wideo
prop-audio-codec = Kodek audio
prop-date-encoded = Data zakodowania
prop-director = Reżyserzy
prop-producer = Producenci
prop-writer = Scenarzyści
prop-stream-count = Strumienie
prop-four-cc = FourCC
prop-author = Autorzy
prop-last-author = Ostatnio zapisany przez
prop-category = Kategoria
prop-company = Firma
prop-manager = Menedżer
prop-page-count = Strony
prop-slide-count = Slajdy
prop-word-count = Liczba wyrazów
prop-char-count = Liczba znaków
prop-line-count = Liczba wierszy
prop-paragraph-count = Liczba akapitów
prop-revision-number = Numer wersji
prop-template = Szablon
prop-date-printed = Data ostatniego wydruku
prop-date-last-saved = Data ostatniego zapisu
prop-content-created = Zawartość utworzona
prop-total-editing-time = Całkowity czas edycji
prop-security = Zabezpieczenia
prop-file-version = Wersja pliku
prop-product-version = Wersja produktu
prop-product-name = Nazwa produktu
prop-file-description = Opis pliku
prop-original-filename = Oryginalna nazwa pliku
prop-internal-name = Nazwa wewnętrzna
prop-legal-trademarks = Znaki towarowe
prop-private-build = Kompilacja prywatna
prop-special-build = Kompilacja specjalna
prop-language = Język
prop-machine = Platforma
prop-subsystem = Podsystem
prop-is-dll = DLL
prop-is-signed = Podpisany
prop-linker-version = Wersja konsolidatora
prop-os-version = Wersja systemu operacyjnego
prop-image-version = Wersja obrazu
sem-err-no-model = Wyszukiwanie semantyczne wymaga pliku modelu { $path } (Opcje → Indeksy → Wyszukiwanie semantyczne → Pobierz model)
sem-err-no-images = Wyszukiwanie obrazów (semimage:) wymaga projektora multimodalnego, którego ta kompilacja nie zawiera
sem-err-no-url = Ten model nie ma adresu pobierania; pobierz go samodzielnie i wskaż jego plik w opcjach
sem-status-idle = Semantyczne: brak folderów
sem-status-no-model = Semantyczne: brak modelu
sem-status-loading = Semantyczne: ładowanie modelu
sem-status-ready = Semantyczne: gotowe ({ $chunks ->
    [one] { $chunks } fragment
    [few] { $chunks } fragmenty
    [many] { $chunks } fragmentów
   *[other] { $chunks } fragmentów
})
sem-status-indexing = Semantyczne: indeksowanie, fragmenty: { $chunks } ({ $percent }%)
sem-status-paused = Semantyczne: wstrzymane
sem-status-error = Semantyczne: { $error }
sem-status-download = Semantyczne: pobieranie modelu ({ $percent }%)
sem-download-cancelled = Anulowano pobieranie
sem-download-bad-size = Pobrany plik ma rozmiar { $got } B, oczekiwano { $want } B
sem-download-bad-hash = Pobrany plik nie jest zgodny z sumą kontrolną SHA-256
cmd-semantic-setup = Wyszukiwanie semantyczne…
sem-toggle = Semantyczne
sem-toggle-tip = Szukaj także według znaczenia (dodaje ?sem: do każdego wyszukiwania; wyniki są łączone)
sem-download-done = Model semantyczny został pobrany i zweryfikowany
sem-download-failed = Nie udało się pobrać modelu: { $error }
sem-setup-title = Wyszukiwanie semantyczne
sem-setup-intro = Wyszukiwanie semantyczne znajduje pliki według znaczenia („arkusz budżetu z zeszłego kwartału”, „jak zmienić klucz API”) oprócz zwykłego wyszukiwania dosłownego. Działa w całości na tym komputerze: model EmbeddingGemma-2 (około 300 MB) jest ładowany tylko wtedy, gdy jest potrzebny, a wektory zajmują około 0,5–1 KB na fragment. Przetwarzane są tylko poniższe foldery — w tle i z limitem użycia procesora.
sem-setup-unavailable = Ta kompilacja programu Fennec nie obsługuje wyszukiwania semantycznego.
sem-setup-folders = Foldery:
sem-setup-folders-hint = Oddzielaj foldery znakiem ; — puste pole oznacza foldery Dokumenty i Pulpit.
sem-setup-model = Model: { $name }
sem-setup-model-ok = Plik modelu jest dostępny.
sem-setup-download = Pobierz model ({ $mb } MB)
sem-setup-stats = Pliki: { $docs }, fragmenty: { $chunks }, w pamięci: { $size }
sem-setup-enable-first = Włącz wyszukiwanie semantyczne, aby wybrać model.
sem-setup-enable = Włącz wyszukiwanie semantyczne
sem-setup-apply = Zastosuj
sem-setup-rebuild = Odbuduj indeks semantyczny
sem-setup-loaded = Model jest w pamięci.
sem-setup-unloaded = Modelu nie ma w pamięci (zostanie załadowany przy następnym wyszukiwaniu semantycznym).
status-http = HTTP: { $url }
status-http-failed = Serwer HTTP: { $error }
msg-http-bad-bind = Nieprawidłowy adres powiązania HTTP: { $value }
msg-everything-running = Everything jest uruchomiony: nie utworzono okna IPC zgodnego z Everything.
msg-http-no-password = wymagane jest hasło (Opcje → Serwery)
msg-http-bind-failed = nie można nasłuchiwać na { $addr }: { $error }
msg-ipc-failed = Nie uruchomiono serwera IPC: { $error }
http-search = Szukaj
http-results = Wyniki: { $count }
http-previous = Poprzednia
http-next = Następna
cmd-advanced-rename = Zaawansowana zmiana nazwy…
cmd-undo = Cofnij
cmd-redo = Ponów
cmd-undo-action = Cofnij: { $action }
cmd-redo-action = Ponów: { $action }
undo-delete = usunięcie
undo-rename = zmiana nazwy
undo-new-folder = nowy folder
undo-advanced-rename = zaawansowana zmiana nazwy
msg-undone = Cofnięto: { $action }
msg-redone = Ponowiono: { $action }
msg-nothing-to-undo = Nie ma czego cofnąć
msg-nothing-to-redo = Nie ma czego ponowić
msg-renamed-many = { $count ->
    [one] Zmieniono nazwę { $count } elementu
    [few] Zmieniono nazwy { $count } elementów
    [many] Zmieniono nazwy { $count } elementów
   *[other] Zmieniono nazwy { $count } elementów
}
rename-title = Zaawansowana zmiana nazwy ({ $count ->
    [one] { $count } element
    [few] { $count } elementy
    [many] { $count } elementów
   *[other] { $count } elementów
})
rename-template = Nowa nazwa:
rename-template-help = $name: $stem: $ext: $parent: $size: $dm: $dc: i $n (licznik)
rename-find = Znajdź:
rename-replace = Zamień na:
rename-regex = Wyrażenie regularne
rename-match-case = Uwzględniaj wielkość liter
rename-keep-ext = Zachowaj rozszerzenie
rename-trim = Usuń zbędne spacje
rename-case = Wielkość liter:
rename-case-keep = Bez zmian
rename-case-lower = małe litery
rename-case-upper = WIELKIE LITERY
rename-case-title = Każdy Wyraz Wielką Literą
rename-case-sentence = Jak w zdaniu
rename-counter = Licznik ($n):
rename-start = początek
rename-step = krok
rename-digits = cyfry
rename-old = Bieżąca nazwa
rename-new = Nowa nazwa
rename-apply = { $count ->
    [one] Zmień nazwę { $count } elementu
    [few] Zmień nazwy { $count } elementów
    [many] Zmień nazwy { $count } elementów
   *[other] Zmień nazwy { $count } elementów
}
rename-problems = { $count ->
    [one] Nie można użyć { $count } nazwy
    [few] Nie można użyć { $count } nazw
    [many] Nie można użyć { $count } nazw
   *[other] Nie można użyć { $count } nazw
}
rename-problem-empty = pusta nazwa
rename-problem-invalid = nieprawidłowe znaki
rename-problem-duplicate = zduplikowana nazwa
rename-problem-exists = już istnieje
rename-problem-pattern = nieprawidłowe wyrażenie regularne
tab-new = Nowa karta
cmd-new-tab = Nowa karta
cmd-close-tab = Zamknij kartę
cmd-next-tab = Następna karta
cmd-previous-tab = Poprzednia karta
cmd-find-in-results = Znajdź w wynikach…
cmd-find-next = Znajdź następny
cmd-find-previous = Znajdź poprzedni
find-label = Znajdź:
find-case = Uwzględniaj wielkość liter
find-whole-word = Cały wyraz
find-regex = Wyrażenie regularne
find-next = Następny (F3)
find-previous = Poprzedni (Shift+F3)
find-none = Nie znaleziono
find-count = { $count ->
    [one] { $count } dopasowanie
    [few] { $count } dopasowania
    [many] { $count } dopasowań
   *[other] { $count } dopasowań
}
find-close = Zamknij (Esc)
cmd-navigate-back = Wstecz
cmd-navigate-forward = Dalej
cmd-toggle-sidebar = Pasek boczny
cmd-toggle-toolbar = Pasek narzędzi
sidebar-folders = Foldery
sidebar-bookmarks = Zakładki
sidebar-no-bookmarks = Brak zakładek (Zakładki → Dodaj do zakładek).
cmd-theme-high-contrast = Duży kontrast
col-relevance = Trafność
col-custom = Niestandardowa { $n }
cmd-rebuild-index = Odbuduj indeks
msg-rebuilding-index = Odbudowywanie indeksu; do zakończenia używany jest bieżący indeks
options-password-set = (ustawione — wpisz, aby zmienić)
undo-copy = kopiowanie
undo-move = przenoszenie
