//! Response bodies: Everything-compatible JSON and the built-in HTML page.

use fnc_ipc::format::{DateFormat, SizeFormat, format_filetime, format_size};
use fnc_ipc::proto::{Column, Results, UNKNOWN};

use crate::params::{SearchParams, download_url, encode_component, html_escape, json_escape};
use crate::{HttpColumns, HttpLabels};

/// Everything's JSON layout:
///
/// ```text
/// {
/// "totalResults":2,
/// "results":[
///     {
///         "type":"folder",
///         "name":"Docs",
///         "path":"C:\\Users",
///         "date_modified":"133000000000000000"
///     },
///     ...
/// ]
/// }
/// ```
///
/// Only requested columns are present; sizes, dates and attributes are decimal strings.
/// SPEC-GAP: unknown values (folder sizes, missing dates) are omitted rather than sent
/// as empty strings.
pub fn json(p: &SearchParams, r: &Results) -> String {
    let mut out = String::with_capacity(64 + r.len() * 160);
    out.push_str(&format!("{{\r\n\"totalResults\":{},\r\n\"results\":[\r\n", r.total));
    for i in 0..r.len() {
        let row = r.row(i);
        out.push_str("\t{\r\n");
        let mut fields: Vec<(&str, String)> = vec![("type", if row.is_folder() { "folder" } else { "file" }.into())];
        fields.push(("name", json_escape(&row.name)));
        if p.path_column {
            fields.push(("path", json_escape(&row.path)));
        }
        if p.size_column && row.size != UNKNOWN && !row.is_folder() {
            fields.push(("size", row.size.to_string()));
        }
        if p.date_modified_column && row.date_modified != UNKNOWN {
            fields.push(("date_modified", row.date_modified.to_string()));
        }
        if p.date_created_column && row.date_created != UNKNOWN {
            fields.push(("date_created", row.date_created.to_string()));
        }
        if p.attributes_column && row.attributes != u32::MAX {
            fields.push(("attributes", row.attributes.to_string()));
        }
        let n = fields.len();
        for (j, (k, v)) in fields.into_iter().enumerate() {
            out.push_str(&format!("\t\t\"{k}\":\"{v}\"{}\r\n", if j + 1 < n { "," } else { "" }));
        }
        out.push_str(if i + 1 < r.len() { "\t},\r\n" } else { "\t}\r\n" });
    }
    out.push_str("]\r\n}\r\n");
    out
}

/// Columns the HTML page shows (name always).
pub fn html_columns(c: &HttpColumns) -> Vec<Column> {
    let mut v = vec![Column::Name];
    for (on, col) in [
        (c.path, Column::Path),
        (c.size, Column::Size),
        (c.date_modified, Column::DateModified),
        (c.date_created, Column::DateCreated),
        (c.attributes, Column::Attributes),
    ] {
        if on {
            v.push(col);
        }
    }
    v
}

const STYLE: &str = "body{font:14px system-ui,-apple-system,'Segoe UI',sans-serif;margin:0;color:#222;background:#fff}\
header{position:sticky;top:0;background:#f4f4f4;border-bottom:1px solid #ddd;padding:8px 12px}\
form{display:flex;gap:6px}input[type=search]{flex:1;font-size:15px;padding:5px 8px}\
table{border-collapse:collapse;width:100%}th,td{padding:3px 12px;text-align:left;white-space:nowrap;\
overflow:hidden;text-overflow:ellipsis;max-width:40vw}th{background:#fafafa;border-bottom:1px solid #ddd;\
font-weight:600}th a{color:inherit;text-decoration:none}tr:nth-child(even){background:#f9f9f9}\
td.num{text-align:right}a{color:#0645ad}.status,.pages{padding:6px 12px;color:#555}\
@media(prefers-color-scheme:dark){body{background:#1e1e1e;color:#ddd}header,th{background:#2a2a2a;\
border-color:#444}tr:nth-child(even){background:#252525}a{color:#8ab4f8}.status,.pages{color:#aaa}}";

/// Renders the search page.
pub fn html(
    p: &SearchParams,
    r: Option<&Results>,
    cols: &[Column],
    labels: &HttpLabels,
    count: u64,
    error: Option<&str>,
) -> String {
    let title = if p.search.is_empty() { "Fennec".to_string() } else { format!("{} - Fennec", html_escape(&p.search)) };
    let mut out = String::with_capacity(4096);
    out.push_str("<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">");
    out.push_str(&format!("<title>{title}</title><style>{STYLE}</style></head><body>\n<header>"));
    out.push_str(&format!(
        "<form method=\"get\" action=\"/\"><input type=\"search\" name=\"search\" value=\"{}\" autofocus \
         aria-label=\"{}\"><input type=\"submit\" value=\"{}\"></form></header>\n",
        html_escape(&p.search),
        html_escape(&labels.search),
        html_escape(&labels.search)
    ));
    if let Some(e) = error {
        out.push_str(&format!("<p class=\"status\">{}</p>\n", html_escape(e)));
    }
    if let Some(r) = r {
        out.push_str(&format!(
            "<p class=\"status\">{}</p>\n<table><thead><tr>",
            html_escape(&labels.results.replace("{count}", &fnc_ipc::format::format_number(r.total, true)))
        ));
        for &c in cols {
            let asc = if p.sort == c { !p.ascending } else { true };
            let arrow = if p.sort == c { if p.ascending { " \u{25B2}" } else { " \u{25BC}" } } else { "" };
            out.push_str(&format!(
                "<th><a href=\"{}\">{}{arrow}</a></th>",
                html_escape(&p.link(0, c, asc)),
                html_escape(labels.column(c))
            ));
        }
        out.push_str("</tr></thead><tbody>\n");
        for i in 0..r.len() {
            let row = r.row(i);
            out.push_str("<tr>");
            for &c in cols {
                match c {
                    Column::Name => {
                        let href = if row.is_folder() {
                            let sep = if row.full_path.starts_with('/') { '/' } else { '\\' };
                            let target = if row.full_path.ends_with(['\\', '/']) {
                                row.full_path.clone()
                            } else {
                                format!("{}{sep}", row.full_path)
                            };
                            format!("/?search={}", encode_component(&format!("\"{target}\"")))
                        } else {
                            download_url(&row.full_path)
                        };
                        out.push_str(&format!(
                            "<td><a href=\"{}\">{}</a></td>",
                            html_escape(&href),
                            html_escape(&row.name)
                        ));
                    }
                    Column::Path => out.push_str(&format!("<td>{}</td>", html_escape(&row.path))),
                    Column::Size => out.push_str(&format!(
                        "<td class=\"num\">{}</td>",
                        html_escape(&format_size(row.size, SizeFormat::Auto, true))
                    )),
                    Column::DateModified | Column::DateCreated => {
                        let v = if c == Column::DateModified { row.date_modified } else { row.date_created };
                        out.push_str(&format!("<td>{}</td>", html_escape(&format_filetime(v, DateFormat::Default))));
                    }
                    Column::Attributes => out.push_str(&format!(
                        "<td>{}</td>",
                        html_escape(&fnc_ipc::format::format_attributes(if row.attributes == u32::MAX {
                            0
                        } else {
                            row.attributes
                        }))
                    )),
                    _ => out.push_str("<td></td>"),
                }
            }
            out.push_str("</tr>\n");
        }
        out.push_str("</tbody></table>\n<p class=\"pages\">");
        if p.offset > 0 {
            let prev = p.offset.saturating_sub(count);
            out.push_str(&format!(
                "<a href=\"{}\" rel=\"prev\">{}</a> ",
                html_escape(&p.link(prev, p.sort, p.ascending)),
                html_escape(&labels.previous)
            ));
        }
        let next = p.offset.saturating_add(r.len() as u64);
        if next < r.total {
            out.push_str(&format!(
                "<a href=\"{}\" rel=\"next\">{}</a>",
                html_escape(&p.link(next, p.sort, p.ascending)),
                html_escape(&labels.next)
            ));
        }
        out.push_str("</p>\n");
    }
    out.push_str("</body></html>\n");
    out
}
