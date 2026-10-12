app-title = Fennec
search-hint = Search
col-name = Name
col-path = Path
col-size = Size
col-modified = Date Modified
status-objects = { $count ->
    [one] 1 object
   *[other] { $count } objects
}
status-selected = { $count } selected
status-elapsed = { $ms } ms
index-starting = Starting…
index-building = Indexing { $volume }: { $done } entries
index-meta = Reading sizes and dates { $percent }%
index-ready = { $count } entries indexed in { $seconds } s
index-error = Index error: { $message }
index-watch-error = Monitoring { $volume } stopped: { $message }
index-folder-fallback = No access to the NTFS journal of { $volume } (run as administrator or install the service); indexing it as a folder.
empty-results = No results
empty-indexing = Building the index…
action-open = Open
action-open-path = Open Path
action-copy-path = Copy Full Path
action-copy-name = Copy Name
tray-show = Show Fennec
tray-exit = Exit
msg-copied = Copied to clipboard
msg-open-failed = Could not open { $path }: { $error }
diag-unsupported = Not supported yet: { $name }:
diag-invalid = Invalid value for { $name }: { $value }
diag-pattern = Invalid pattern: { $message }
diag-quote = Unclosed quote
diag-group = Unclosed <
