app-title = Fennec
search-hint = Поиск
col-name = Имя
col-path = Путь
col-size = Размер
col-modified = Дата изменения
status-objects = { $count ->
    [one] { $count } объект
    [few] { $count } объекта
    [many] { $count } объектов
   *[other] { $count } объектов
}
status-selected = Выбрано: { $count }
status-elapsed = { $ms } ms
index-starting = Запуск…
index-building = Индексация { $volume }: обработано { $done }
index-meta = Чтение размеров и дат: { $percent }%
index-ready = Проиндексировано записей: { $count } за { $seconds } s
index-error = Ошибка индекса: { $message }
index-db-rebuild = Не удалось прочитать базу данных индекса, она создается заново ({ $message }).
index-volume-rebuild = Журнал изменений тома { $volume } изменился; том индексируется заново.
index-volume-added = { $volume } добавлен в индекс.
index-watch-error = Отслеживание { $volume } остановлено: { $message }
index-folder-fallback = Нет доступа к журналу NTFS тома { $volume } (запустите от имени администратора или установите службу); том индексируется как папка.
empty-results = Нет результатов
empty-indexing = Построение индекса…
action-open = Открыть
action-open-path = Открыть путь
action-copy-path = Копировать полный путь
action-copy-name = Копировать имя
tray-show = Показать Fennec
tray-exit = Выход
msg-copied = Скопировано в буфер обмена
msg-open-failed = Не удалось открыть { $path }: { $error }
diag-unsupported = Пока не поддерживается: { $name }:
diag-invalid = Недопустимое значение { $name }: { $value }
diag-pattern = Недопустимый шаблон: { $message }
diag-quote = Незакрытая кавычка
diag-group = Незакрытая скобка <
msg-restart-required = Перезапустите Fennec, чтобы изменить индексируемые папки и списки файлов.
cmd-focus-search = Перейти к поиску
cmd-refresh = Обновить
cmd-open = Открыть
cmd-open-path = Открыть путь
cmd-open-with = Открыть с помощью…
cmd-run-as-admin = Запуск от имени администратора
cmd-properties = Свойства
cmd-context-menu = Контекстное меню
cmd-copy = Копировать
cmd-cut = Вырезать
cmd-paste = Вставить
cmd-copy-full-path = Копировать полный путь
cmd-copy-full-path-quoted = Копировать полный путь (в кавычках)
cmd-copy-name = Копировать имя
cmd-copy-path = Копировать путь
cmd-copy-as-tsv = Копировать как TSV
cmd-copy-as-csv = Копировать как CSV
cmd-copy-as-efu = Копировать как список файлов
cmd-delete = Удалить
cmd-delete-permanently = Удалить безвозвратно
cmd-rename = Переименовать
cmd-new-folder = Новая папка
cmd-explore-in-fennec = Обзор в Fennec
cmd-omit = Исключить из результатов
cmd-set-run-count = Задать число запусков…
cmd-select-all = Выделить все
cmd-invert-selection = Обратить выделение
cmd-toggle-selection = Переключить выделение
cmd-move-down = Вниз
cmd-move-up = Вверх
cmd-extend-down = Расширить выделение вниз
cmd-extend-up = Расширить выделение вверх
cmd-focus-down = Фокус вниз
cmd-focus-up = Фокус вверх
cmd-page-down = На страницу вниз
cmd-page-up = На страницу вверх
cmd-first = Первый результат
cmd-last = Последний результат
cmd-match-case = Учитывать регистр
cmd-match-whole-word = Только слова целиком
cmd-match-path = Искать в пути
cmd-match-diacritics = Учитывать диакритические знаки
cmd-enable-regex = Регулярные выражения
cmd-advanced-search = Расширенный поиск…
cmd-add-bookmark = Добавить в закладки…
cmd-organize-bookmarks = Упорядочить закладки…
cmd-organize-filters = Упорядочить фильтры…
cmd-add-filter = Добавить фильтр…
cmd-filter-everything = Всё
cmd-options = Параметры…
cmd-export = Экспорт…
cmd-toggle-menu-bar = Строка меню
cmd-toggle-status-bar = Строка состояния
cmd-toggle-filter-bar = Панель фильтров
cmd-theme-light = Светлая
cmd-theme-dark = Темная
cmd-theme-system = Как в системе
cmd-zoom-in = Увеличить
cmd-zoom-out = Уменьшить
cmd-zoom-reset = Сбросить масштаб
cmd-search-history = История поиска
cmd-clear-history = Очистить историю поиска
cmd-install-service = Установить службу Fennec
cmd-uninstall-service = Удалить службу Fennec
cmd-save-database = Сохранить индекс сейчас
cmd-pause-updates = Приостановить обновление индекса
cmd-help = Синтаксис поиска
cmd-about = О программе Fennec
cmd-hide-window = Скрыть окно
cmd-exit = Выход
cmd-filter-1 = Фильтр 1
cmd-filter-2 = Фильтр 2
cmd-filter-3 = Фильтр 3
cmd-filter-4 = Фильтр 4
cmd-filter-5 = Фильтр 5
cmd-filter-6 = Фильтр 6
cmd-filter-7 = Фильтр 7
cmd-filter-8 = Фильтр 8
cmd-filter-9 = Фильтр 9
col-ext = Расширение
col-type = Тип
col-dm = Дата изменения
col-dc = Дата создания
col-da = Дата доступа
col-attributes = Атрибуты
col-runcount = Число запусков
col-dr = Дата запуска
col-rc = Дата недавнего изменения
col-fullpath = Полный путь
col-filelist = Имя файла списка
col-indexsource = Источник индекса
col-volume = Том
col-depth = Глубина
col-namelength = Длина имени
col-frn = Номер ссылки на файл
options-page-general = Общие
options-desc-general = Язык, единственный экземпляр, область уведомлений и горячая клавиша.
options-page-ui = Интерфейс
options-desc-ui = Тема, масштаб, высота строк и отображаемые панели.
options-page-search = Поиск
options-desc-search = Как выполняется сопоставление по умолчанию.
options-page-results = Результаты
options-desc-results = Подсветка, порядок папок и открытие.
options-page-view = Вид
options-desc-view = Столбцы, порядок сортировки и форматы.
options-page-fonts_colors = Шрифты и цвета
options-desc-fonts_colors =  
options-page-history = История
options-desc-history = История поиска и запусков.
options-page-keyboard = Клавиатура
options-desc-keyboard =  
options-page-context_menu = Контекстное меню
options-desc-context_menu = Интеграция с Проводником устанавливается вместе с пунктами контекстного меню (fennec -install-shell, M4).
options-page-open_commands = Команды открытия
options-desc-open_commands = Свои команды открытия: Название|программа аргументы, %1 = полный путь.
options-page-index = Индексы
options-desc-index = Что индексируется и как сохраняется база данных.
options-page-ntfs = NTFS / ReFS
options-desc-ntfs = Тома NTFS/ReFS и их журналы изменений.
options-page-folders = Папки
options-desc-folders = Индексы папок (FAT, сеть, любая папка).
options-page-filelists = Списки файлов
options-desc-filelists = Подключенные списки файлов EFU.
options-page-exclude = Исключения
options-desc-exclude = Скрытые и системные файлы, исключенные папки.
options-page-properties = Свойства
options-desc-properties = Индексирование свойств (ширина, длительность, автор…).
options-page-content = Содержимое
options-desc-content = Индексирование содержимого.
options-page-semantic = Семантический поиск
options-desc-semantic = Локальный семантический поиск с EmbeddingGemma-2. Скачайте модель и проверьте ее состояние в меню Сервис → Семантический поиск…
options-page-index_journal = Журнал индекса
options-desc-index_journal = Журнал индекса (дата недавнего изменения).
options-page-server = Серверы
options-desc-server = IPC-сервер для fes.exe и SDK, IPC-окно, совместимое с Everything (es.exe, средства запуска), и HTTP-сервер.
options-page-advanced = Дополнительно
options-desc-advanced =  
keyctx-search = Поле поиска
keyctx-results = Список результатов
keyctx-global = Глобально
filter-audio = Аудио
filter-compressed = Архивы
filter-document = Документы
filter-executable = Исполняемые
filter-folder = Папки
filter-picture = Изображения
filter-video = Видео
filter-everything = Всё
menu-file = Файл
menu-edit = Правка
menu-view = Вид
menu-search = Поиск
menu-bookmarks = Закладки
menu-tools = Сервис
menu-help = Справка
menu-copy-as = Копировать как
menu-theme = Тема
menu-zoom = Масштаб
menu-columns = Столбцы
menu-reset-columns = Сбросить столбцы
tray-options = Параметры…
tray-pause = Приостановить/возобновить обновление индекса
status-paused = обновления приостановлены
type-folder = Папка с файлами
type-file = Файл
source-folder = Папка
source-filelist = Список файлов
time-just-now = только что
new-folder-name = Новая папка
dialog-ok = ОК
dialog-cancel = Отмена
dialog-close = Закрыть
dialog-add = Добавить…
dialog-edit = Изменить…
dialog-delete = Удалить
dialog-import = Импорт из Everything
dialog-create = Создать
dialog-search = Найти
dialog-save = Сохранить
dialog-edit-filter = Фильтр
dialog-run-count = Число запусков:
dialog-export-help = Экспорт результатов в файл; формат определяется расширением: .efu (список файлов), .csv / .tsv / .json (видимые столбцы), .m3u / .m3u8, любое другое — по одному пути в строке.
field-name = Имя
field-search = Поиск
field-filter = Фильтр
field-sort = Сортировка
field-folder = Папка
field-macro = Макрос
builder-all-words = Все эти слова
builder-exact-phrase = Точная фраза
builder-any-words = Любое из этих слов
builder-none-words = Ни одного из этих слов
builder-match-case = Учитывать регистр
builder-whole-word = Только слова целиком
builder-folder = В папке
builder-subfolders = Включая вложенные папки
builder-extensions = Расширения
builder-kind = Искать
builder-kind-all = Файлы и папки
builder-kind-files = Файлы
builder-kind-folders = Папки
builder-size = Размер
builder-date = Дата изменения
builder-attributes = Атрибуты
builder-hidden = Скрытый
builder-system = Системный
builder-readonly = Только чтение
builder-preview = Запрос:
options-title = Параметры Fennec
options-ok = ОК
options-cancel = Отмена
options-apply = Применить
options-reset = Сбросить
options-reset-keys = Восстановить сочетания клавиш по умолчанию
options-show-containing = Показать параметры, содержащие:
options-empty-page = Здесь пока нечего настраивать.
options-command = Команда
options-press-key = Нажмите клавишу…
options-filter-hint = Фильтр команд
options-keyboard-help = Щелкните сочетание клавиш, чтобы изменить его; ✕ удаляет его.
msg-copied-files = { $count ->
    [one] { $count } объект скопирован в буфер обмена
    [few] { $count } объекта скопировано в буфер обмена
    [many] { $count } объектов скопировано в буфер обмена
   *[other] { $count } объектов скопировано в буфер обмена
}
msg-pasted = Вставлено
msg-op-failed = Ошибка операции: { $error }
msg-deleted = { $count ->
    [one] Удален { $count } объект
    [few] Удалено { $count } объекта
    [many] Удалено { $count } объектов
   *[other] Удалено { $count } объектов
}
msg-renamed = Новое имя: { $name }
msg-created = Создано: { $name }
msg-db-saved = Индекс сохранен
msg-service-requested = Команда службы запущена
msg-history-cleared = История поиска очищена
msg-setting-changed = { $key } = { $value }
msg-unknown-command = Неизвестная команда: /{ $command }
msg-bad-bindings = Недопустимые сочетания клавиш пропущены: { $bindings }
msg-updates-paused = Обновление индекса приостановлено
msg-updates-resumed = Обновление индекса возобновлено
msg-imported = Импортировано из { $file }: { $count }
msg-import-missing = { $file } не найден (поиск выполнялся в папках данных Fennec и Everything)
msg-exported = { $count ->
    [one] { $count } результат экспортирован в { $path }
    [few] { $count } результата экспортировано в { $path }
    [many] { $count } результатов экспортировано в { $path }
   *[other] { $count } результатов экспортировано в { $path }
}
about-text =
    Найдите что угодно в мгновение ока.
    Поиск по именам файлов, совместимый с синтаксисом поиска Everything 1.5.
time-minutes-ago = { $n } мин назад
time-hours-ago = { $n ->
    [one] { $n } час назад
    [few] { $n } часа назад
    [many] { $n } часов назад
   *[other] { $n } часов назад
}
time-days-ago = { $n ->
    [one] { $n } день назад
    [few] { $n } дня назад
    [many] { $n } дней назад
   *[other] { $n } дней назад
}
type-ext-file = Файл "{ $ext }"
dialog-new-folder-in = Новая папка в { $path }:
help-syntax =
    Синтаксис поиска (совместим с Everything)
        пробел   И        |   ИЛИ        !   НЕ       < >   группировка       "…"   буквальный текст
        {"*"}  ?    подстановочные знаки (все имя)   \ в слове — поиск по полному пути   ^abc / abc$ начало / конец имени
    Модификаторы: case: ww: wfn: path: regex: diacritics: nopath: py: (пиньинь) …
    Функции: ext:jpg;png  size:>10mb  dm:today  dc:2026  attrib:H  parent:C:\Windows  child:*.mp3  dupe:  empty:  depth:2 …
    Макросы: audio: zip: doc: exe: pic: video:
    Команды: введите /search_delay=100 или /reindex в поле поиска и нажмите Enter.
dialog-confirm-delete = { $count ->
    [1] Удалить этот объект безвозвратно? Это действие нельзя отменить.
    [one] Удалить безвозвратно { $count } объект? Это действие нельзя отменить.
    [few] Удалить безвозвратно { $count } объекта? Это действие нельзя отменить.
    [many] Удалить безвозвратно { $count } объектов? Это действие нельзя отменить.
   *[other] Удалить безвозвратно { $count } объектов? Это действие нельзя отменить.
}
cmd-index-journal = Журнал индекса…
journal-create = Создание
journal-delete = Удаление
journal-rename = Переименование
journal-move = Перемещение
journal-modify = Изменение
journal-filter = Показать изменения, содержащие:
journal-count = { $count ->
    [one] { $count } запись
    [few] { $count } записи
    [many] { $count } записей
   *[other] { $count } записей
}
journal-time = Время
journal-action = Изменение
journal-before = До
journal-after = После
cmd-toggle-preview = Область просмотра
cmd-view-details = Таблица
cmd-view-medium-thumbnails = Средние эскизы
cmd-view-large-thumbnails = Крупные эскизы
cmd-view-extra-large-thumbnails = Огромные эскизы
cmd-next-item = Следующий элемент
cmd-previous-item = Предыдущий элемент
preview-nothing = Выберите результат для предварительного просмотра.
preview-loading = Загрузка…
preview-binary = Двоичный файл (текстовый просмотр недоступен)
preview-truncated = Показан только первый 1 MB.
preview-selected = { $count ->
    [one] Выбран { $count } объект
    [few] Выбрано { $count } объекта
    [many] Выбрано { $count } объектов
   *[other] Выбрано { $count } объектов
}
preview-total-size = Общий размер: { $size }
preview-contents = Содержимое
preview-children = Файлов: { $files }, папок: { $folders }
cmd-new-window = Новое окно
cmd-close-window = Закрыть окно
msg-unknown-filter = Неизвестный фильтр: { $name }
msg-unknown-bookmark = Неизвестная закладка: { $name }
msg-unknown-sort = Неизвестный столбец сортировки: { $name }
shell-search-fennec = Поиск в Fennec
shell-explore-in-fennec = Обзор в Fennec
cmd-import-settings = Импорт параметров…
cmd-export-settings = Экспорт параметров…
cmd-import-everything-ini = Импорт Everything.ini…
cmd-export-filters = Экспорт фильтров…
cmd-export-bookmarks = Экспорт закладок…
cmd-export-search-history = Экспорт истории поиска…
menu-import-export = Импорт и экспорт
dialog-transfer-path = Файл:
msg-transfer-done = Готово: { $path }
msg-ini-imported = Импортировано параметров из Everything.ini: { $count }
index-extracting = Свойства и содержимое: { $done } / { $total }
warn-not-indexed = { $name }: не индексируется (включите индекс в параметрах или используйте notindexed:{ $name }:)
warn-slow-limit = Медленный поиск остановлен, прочитано файлов: { $count }; добавьте фильтры, чтобы сузить его.
warn-semantic = Семантический поиск: { $reason }
value-yes = Да
value-no = Нет
unit-pixels = { $count ->
    [one] { $count } пиксель
    [few] { $count } пикселя
    [many] { $count } пикселей
   *[other] { $count } пикселей
}
unit-stars = { $count ->
    [one] { $count } звезда
    [few] { $count } звезды
    [many] { $count } звезд
   *[other] { $count } звезд
}
menu-property-columns = Столбцы свойств
col-dupegroup = Группа дубликатов
preview-hits = { $count ->
    [one] { $count } совпадение
    [few] { $count } совпадения
    [many] { $count } совпадений
   *[other] { $count } совпадений
}
prop-width = Ширина
prop-height = Высота
prop-dimensions = Размеры
prop-aspect-ratio = Соотношение сторон
prop-bit-depth = Глубина цвета
prop-horizontal-resolution = Разрешение по горизонтали
prop-vertical-resolution = Разрешение по вертикали
prop-orientation = Ориентация
prop-date-taken = Дата съемки
prop-camera-maker = Производитель камеры
prop-camera-model = Модель камеры
prop-exposure-time = Выдержка
prop-f-stop = Диафрагма
prop-iso-speed = Светочувствительность ISO
prop-focal-length = Фокусное расстояние
prop-flash = Вспышка
prop-latitude = Широта
prop-longitude = Долгота
prop-altitude = Высота над уровнем моря
prop-people = Люди
prop-tags = Теги
prop-rating = Оценка
prop-title = Название
prop-subject = Тема
prop-comment = Комментарии
prop-copyright = Авторские права
prop-length = Продолжительность
prop-artist = Исполнитель
prop-album = Альбом
prop-album-artist = Исполнитель альбома
prop-track = {"#"}
prop-disc = Диск
prop-genre = Жанр
prop-year = Год
prop-composer = Композитор
prop-conductor = Дирижер
prop-publisher = Издатель
prop-bit-rate = Битрейт
prop-sample-rate = Частота дискретизации
prop-channels = Каналы
prop-bits-per-sample = Бит на выборку
prop-is-variable-bit-rate = Переменный битрейт
prop-lyrics = Текст песни
prop-mood = Настроение
prop-bpm = Ударов в минуту
prop-initial-key = Начальная тональность
prop-encoded-by = Кодировщик
prop-frame-width = Ширина кадра
prop-frame-height = Высота кадра
prop-frame-rate = Частота кадров
prop-video-bitrate = Скорость передачи данных
prop-total-bitrate = Общая скорость потока
prop-compression = Сжатие видео
prop-video-codec = Видеокодек
prop-audio-codec = Аудиокодек
prop-date-encoded = Дата кодирования
prop-director = Режиссеры
prop-producer = Продюсеры
prop-writer = Сценаристы
prop-stream-count = Потоки
prop-four-cc = FourCC
prop-author = Авторы
prop-last-author = Кем сохранен
prop-category = Категория
prop-company = Организация
prop-manager = Руководитель
prop-page-count = Страницы
prop-slide-count = Слайды
prop-word-count = Число слов
prop-char-count = Число знаков
prop-line-count = Число строк
prop-paragraph-count = Число абзацев
prop-revision-number = Номер редакции
prop-template = Шаблон
prop-date-printed = Дата последней печати
prop-date-last-saved = Дата последнего сохранения
prop-content-created = Содержимое создано
prop-total-editing-time = Общее время правки
prop-security = Безопасность
prop-file-version = Версия файла
prop-product-version = Версия продукта
prop-product-name = Название продукта
prop-file-description = Описание файла
prop-original-filename = Исходное имя файла
prop-internal-name = Внутреннее имя
prop-legal-trademarks = Товарные знаки
prop-private-build = Частная сборка
prop-special-build = Особая сборка
prop-language = Язык
prop-machine = Платформа
prop-subsystem = Подсистема
prop-is-dll = DLL
prop-is-signed = Подписан
prop-linker-version = Версия компоновщика
prop-os-version = Версия ОС
prop-image-version = Версия образа
sem-err-no-model = Для семантического поиска нужен файл модели { $path } (Параметры → Индексы → Семантический поиск → Скачать модель)
sem-err-no-images = Для поиска изображений (semimage:) нужен мультимодальный проектор, которого нет в этой сборке
sem-err-no-url = У этой модели нет адреса загрузки; скачайте ее вручную и укажите файл в параметрах
sem-status-idle = Семантический поиск: нет папок
sem-status-no-model = Семантический поиск: нет модели
sem-status-loading = Семантический поиск: загрузка модели
sem-status-ready = Семантический поиск: готов ({ $chunks ->
    [one] { $chunks } фрагмент
    [few] { $chunks } фрагмента
    [many] { $chunks } фрагментов
   *[other] { $chunks } фрагментов
})
sem-status-indexing = Семантический поиск: индексация, фрагментов: { $chunks } ({ $percent }%)
sem-status-paused = Семантический поиск: приостановлен
sem-status-error = Семантический поиск: { $error }
sem-status-download = Семантический поиск: загрузка модели ({ $percent }%)
sem-download-cancelled = Загрузка отменена
sem-download-bad-size = Размер загруженного файла: { $got } байт, ожидалось { $want }
sem-download-bad-hash = Загруженный файл не совпадает с контрольной суммой SHA-256
cmd-semantic-setup = Семантический поиск…
sem-toggle = Семантика
sem-toggle-tip = Искать также по смыслу (добавляет ?sem: к каждому запросу; результаты объединяются)
sem-download-done = Семантическая модель загружена и проверена
sem-download-failed = Не удалось загрузить модель: { $error }
sem-setup-title = Семантический поиск
sem-setup-intro = Семантический поиск находит файлы по смыслу («бюджет за прошлый квартал», «как сменить ключ API») в дополнение к обычному буквальному поиску. Он полностью работает на этом компьютере: модель EmbeddingGemma-2 (около 300 MB) загружается только при необходимости, а векторы занимают около 0,5–1 KB на фрагмент. Обрабатываются только указанные ниже папки — в фоновом режиме с ограничением нагрузки на ЦП.
sem-setup-unavailable = В этой сборке Fennec нет семантического поиска.
sem-setup-folders = Папки:
sem-setup-folders-hint = Разделяйте папки символом ; — если пусто, используются папки «Документы» и «Рабочий стол».
sem-setup-model = Модель: { $name }
sem-setup-model-ok = Файл модели на месте.
sem-setup-download = Скачать модель ({ $mb } MB)
sem-setup-stats = Файлов: { $docs }, фрагментов: { $chunks }, в памяти: { $size }
sem-setup-enable-first = Включите семантический поиск, чтобы выбрать модель.
sem-setup-enable = Включить семантический поиск
sem-setup-apply = Применить
sem-setup-rebuild = Перестроить семантический индекс
sem-setup-loaded = Модель загружена в память.
sem-setup-unloaded = Модель не загружена в память (она загрузится при следующем семантическом поиске).
status-http = HTTP: { $url }
status-http-failed = HTTP-сервер: { $error }
msg-http-bad-bind = Недопустимый адрес привязки HTTP: { $value }
msg-everything-running = Запущен Everything: IPC-окно, совместимое с Everything, не создано.
msg-http-no-password = требуется пароль (Параметры → Серверы)
msg-http-bind-failed = не удается прослушивать { $addr }: { $error }
msg-ipc-failed = IPC-сервер не запущен: { $error }
http-search = Поиск
http-results = Результатов: { $count }
http-previous = Назад
http-next = Далее
cmd-advanced-rename = Расширенное переименование…
cmd-undo = Отменить
cmd-redo = Повторить
cmd-undo-action = Отменить: { $action }
cmd-redo-action = Повторить: { $action }
undo-delete = удаление
undo-rename = переименование
undo-new-folder = создание папки
undo-advanced-rename = расширенное переименование
msg-undone = Отменено: { $action }
msg-redone = Повторено: { $action }
msg-nothing-to-undo = Нечего отменять
msg-nothing-to-redo = Нечего повторять
msg-renamed-many = { $count ->
    [one] Переименован { $count } объект
    [few] Переименовано { $count } объекта
    [many] Переименовано { $count } объектов
   *[other] Переименовано { $count } объектов
}
rename-title = Расширенное переименование ({ $count ->
    [one] { $count } объект
    [few] { $count } объекта
    [many] { $count } объектов
   *[other] { $count } объектов
})
rename-template = Новое имя:
rename-template-help = $name: $stem: $ext: $parent: $size: $dm: $dc: и $n (счетчик)
rename-find = Найти:
rename-replace = Заменить на:
rename-regex = Регулярное выражение
rename-match-case = Учитывать регистр
rename-keep-ext = Сохранить расширение
rename-trim = Обрезать пробелы
rename-case = Регистр:
rename-case-keep = Без изменений
rename-case-lower = нижний регистр
rename-case-upper = ВЕРХНИЙ РЕГИСТР
rename-case-title = Каждое Слово С Прописной
rename-case-sentence = Как в предложении
rename-counter = Счетчик ($n):
rename-start = начало
rename-step = шаг
rename-digits = цифр
rename-old = Текущее имя
rename-new = Новое имя
rename-apply = { $count ->
    [one] Переименовать { $count } объект
    [few] Переименовать { $count } объекта
    [many] Переименовать { $count } объектов
   *[other] Переименовать { $count } объектов
}
rename-problems = { $count ->
    [one] { $count } имя нельзя использовать
    [few] { $count } имени нельзя использовать
    [many] { $count } имен нельзя использовать
   *[other] { $count } имен нельзя использовать
}
rename-problem-empty = пустое имя
rename-problem-invalid = недопустимые символы
rename-problem-duplicate = повторяющееся имя
rename-problem-exists = уже существует
rename-problem-pattern = недопустимое регулярное выражение
tab-new = Новая вкладка
cmd-new-tab = Новая вкладка
cmd-close-tab = Закрыть вкладку
cmd-next-tab = Следующая вкладка
cmd-previous-tab = Предыдущая вкладка
cmd-find-in-results = Найти в результатах…
cmd-find-next = Найти далее
cmd-find-previous = Найти ранее
find-label = Найти:
find-case = Учитывать регистр
find-whole-word = Слово целиком
find-regex = Регулярное выражение
find-next = Далее (F3)
find-previous = Назад (Shift+F3)
find-none = Не найдено
find-count = { $count ->
    [one] { $count } совпадение
    [few] { $count } совпадения
    [many] { $count } совпадений
   *[other] { $count } совпадений
}
find-close = Закрыть (Esc)
cmd-navigate-back = Назад
cmd-navigate-forward = Вперед
cmd-toggle-sidebar = Боковая панель
cmd-toggle-toolbar = Панель инструментов
sidebar-folders = Папки
sidebar-bookmarks = Закладки
sidebar-no-bookmarks = Закладок пока нет (Закладки → Добавить в закладки).
cmd-theme-high-contrast = Высокая контрастность
