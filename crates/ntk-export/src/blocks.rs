//! Дерево блоков Notion в текст — перенос того, что печатает `ntk show`.
//!
//! Правило здесь одно и оно же было причиной половины прошлых дефектов:
//! ничего не исчезает молча. У типов, чей текст лежит не в `rich_text`, своя
//! ветка; контейнеры без собственного текста прозрачны; всё, до чего не
//! удалось дотянуться при чтении, объявляется отдельной строкой.
//!
//! Перенос сверяется с исходной реализацией тестом на эквивалентность: тихо
//! разошедшийся рендер выглядит не как падение, а как тикет, у которого
//! пропала таблица.

use serde_json::Value;

const ROMAN: &[(u32, &str)] = &[
    (1000, "m"), (900, "cm"), (500, "d"), (400, "cd"), (100, "c"), (90, "xc"),
    (50, "l"), (40, "xl"), (10, "x"), (9, "ix"), (5, "v"), (4, "iv"), (1, "i"),
];

fn rich_text(v: Option<&Value>) -> String {
    let Some(arr) = v.and_then(Value::as_array) else { return String::new() };
    arr.iter()
        .map(|r| {
            r.get("plain_text")
                .and_then(Value::as_str)
                .or_else(|| r.pointer("/text/content").and_then(Value::as_str))
                .unwrap_or("")
        })
        .collect()
}

fn block_type(b: &Value) -> &str {
    b.get("type").and_then(Value::as_str).unwrap_or("")
}

fn payload<'a>(b: &'a Value) -> Option<&'a Value> {
    b.get(block_type(b))
}

fn block_text(b: &Value) -> String {
    rich_text(payload(b).and_then(|p| p.get("rich_text")))
}

/// Notion умеет нумеровать список буквами и римскими цифрами; печатать вместо
/// них арабские значило бы соврать о документе.
fn list_marker(n: u32, format: Option<&str>) -> String {
    match format {
        Some("letters") => {
            let mut out = String::new();
            let mut v = n;
            while v > 0 {
                out.insert(0, (b'a' + ((v - 1) % 26) as u8) as char);
                v = (v - 1) / 26;
            }
            out
        }
        Some("roman") => {
            let mut out = String::new();
            let mut v = n;
            for (value, sym) in ROMAN {
                while v >= *value {
                    out.push_str(sym);
                    v -= value;
                }
            }
            out
        }
        _ => n.to_string(),
    }
}

/// Подпись медиа-блока: сначала его собственная, затем имя файла, затем имя из
/// ссылки. Подпись ОПИСЫВАЕТ файл, но не заменяет знание, какой это файл —
/// ссылку читатель восстановить не сможет.
fn media_label(b: &Value) -> String {
    let d = payload(b).cloned().unwrap_or(Value::Null);
    let caption = rich_text(d.get("caption"));
    let external = d.pointer("/external/url").and_then(Value::as_str);
    let file_url = d.pointer("/file/url").and_then(Value::as_str);
    let name = d.get("name").and_then(Value::as_str);

    let last_segment = |u: &str| -> String {
        u.split('?').next().unwrap_or(u).split('/').filter(|s| !s.is_empty()).next_back().unwrap_or("").to_string()
    };

    if !caption.is_empty() {
        let named = name.map(str::to_string)
            .or_else(|| external.map(str::to_string))
            .or_else(|| file_url.map(|u| last_segment(u)))
            .filter(|s| !s.is_empty());
        return match named {
            Some(n) => format!("{caption} — {n}"),
            None => caption,
        };
    }
    if let Some(n) = name { return n.to_string(); }
    // Файл, который хранит Notion, назван последним сегментом пути; подписанная
    // ссылка в терминале — мусор. Внешняя ссылка называется собой: взять её
    // последний сегмент значило бы напечатать "watch" вместо адреса.
    if let Some(e) = external { return e.to_string(); }
    let url = file_url.or_else(|| d.get("url").and_then(Value::as_str)).unwrap_or("");
    let seg = last_segment(url);
    if seg.is_empty() { url.to_string() } else { seg }
}

fn labelled(kind: &str, text: &str) -> String {
    if text.is_empty() { format!("[{kind}]") } else { format!("[{kind}] {text}") }
}

/// Ставит префикс перед каждой строкой блока. Пустая строка получает только
/// значимую часть префикса: пустая строка внутри цитаты это ">", а не "> ".
fn prefix_lines(text: &str, prefix: &str) -> String {
    text.split('\n')
        .map(|l| if l.is_empty() { prefix.trim_end().to_string() } else { format!("{prefix}{l}") })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Почему дети блока отсутствуют в выводе. Проставляется на стороне чтения;
/// печатается затем, чтобы укороченное тело никогда не сошло за целое.
pub fn elision_reason(kind: &str) -> &'static str {
    match kind {
        "depth" => "nesting too deep",
        "budget" => "request budget exhausted",
        _ => "could not be loaded",
    }
}

pub fn blocks_to_text(blocks: &[Value], indent: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    // Нумерованные пункты печатаются своим настоящим номером: печатать каждый
    // как "1." значит потерять порядок шагов — та же жалоба, с которой всё
    // начиналось, только на другом типе блока.
    let mut ordinal: u32 = 0;
    let mut list_format: Option<String> = None;

    for b in blocks {
        let ty = block_type(b);
        if ty != "numbered_list_item" {
            ordinal = 0;
            list_format = None;
        } else {
            // Перезапуск нумерации может быть отмечен на любом пункте, не
            // только на первом, поэтому номер берётся там, где встретился.
            let start = payload(b).and_then(|p| p.get("list_start_index")).and_then(Value::as_u64);
            ordinal = match start { Some(s) => s as u32, None => ordinal + 1 };
            if let Some(f) = payload(b).and_then(|p| p.get("list_format")).and_then(Value::as_str) {
                list_format = Some(f.to_string());
            }
        }

        let text = block_text(b);
        let out: Option<String> = match ty {
            "heading_1" => Some(format!("\n# {text}")),
            "heading_2" => Some(format!("\n## {text}")),
            "heading_3" => Some(format!("\n### {text}")),
            "heading_4" => Some(format!("\n#### {text}")),
            "bulleted_list_item" => Some(format!("- {text}")),
            "numbered_list_item" => {
                Some(format!("{}. {text}", list_marker(ordinal, list_format.as_deref())))
            }
            "to_do" => {
                let checked = payload(b).and_then(|p| p.get("checked")).and_then(Value::as_bool).unwrap_or(false);
                Some(format!("- [{}] {text}", if checked { "x" } else { " " }))
            }
            "quote" => Some(prefix_lines(&text, "> ")),
            // Пустой абзац — намеренная пустая строка, поэтому он выживает;
            // прочие блоки без текста добавили бы только шум.
            "paragraph" => Some(text),
            "code" => {
                // Забор длиннее любой последовательности обратных кавычек в
                // теле: иначе забор внутри кода закрыл бы блок раньше времени.
                let longest = text.split('`').skip(1).fold(0usize, |acc, part| {
                    let run = text.len() - part.len();
                    acc.max(run.min(text.matches('`').count()))
                });
                let mut fence_len = 3.max(longest + 1);
                while text.contains(&"`".repeat(fence_len)) { fence_len += 1; }
                let fence = "`".repeat(fence_len);
                let lang = payload(b).and_then(|p| p.get("language")).and_then(Value::as_str)
                    .filter(|l| *l != "plain text").unwrap_or("");
                Some(format!("{fence}{lang}\n{text}\n{fence}"))
            }
            "divider" => Some("---".to_string()),
            // Ниже — блоки, чей текст лежит не в rich_text. Без своей ветки
            // каждый рендерится в ничто: именно так из `ntk show` исчезали
            // связанная страница, таблица целиком и приложенная картинка.
            "table_row" => {
                let cells = payload(b).and_then(|p| p.get("cells")).and_then(Value::as_array).cloned().unwrap_or_default();
                Some(cells.iter().map(|c| rich_text(Some(c))).collect::<Vec<_>>().join(" | "))
            }
            "child_page" => Some(format!("[child page: {}]",
                payload(b).and_then(|p| p.get("title")).and_then(Value::as_str).unwrap_or(""))),
            "child_database" => Some(format!("[child database: {}]",
                payload(b).and_then(|p| p.get("title")).and_then(Value::as_str).unwrap_or(""))),
            "image" | "video" | "audio" | "pdf" | "file" => {
                Some(format!("[{ty}: {}]", media_label(b)))
            }
            "bookmark" | "embed" | "link_preview" => {
                // Адрес показывается всегда: подпись, вытеснившая его, скрыла
                // бы, куда ссылка на самом деле ведёт.
                let caption = rich_text(payload(b).and_then(|p| p.get("caption")));
                let url = payload(b).and_then(|p| p.get("url")).and_then(Value::as_str).unwrap_or("");
                Some(if caption.is_empty() { format!("[{ty}: {url}]") } else { format!("[{ty}: {caption} — {url}]") })
            }
            "equation" => Some(labelled("equation",
                payload(b).and_then(|p| p.get("expression")).and_then(Value::as_str).unwrap_or(""))),
            // Тогглы и выноски несут rich_text, но напечатанные голыми читаются
            // как обычные абзацы — та же жалоба, с которой всё начиналось.
            "toggle" => Some(labelled("toggle", &text)),
            "template" => Some(labelled("template", &text)),
            "callout" => Some(labelled("callout", &text)),
            "link_to_page" => {
                let kind = payload(b).and_then(|p| p.get("type")).and_then(Value::as_str).unwrap_or("");
                let target = payload(b).and_then(|p| p.get(kind)).and_then(Value::as_str);
                let shown = kind.replace("_id", "");
                let shown = if shown.is_empty() { "page".to_string() } else { shown };
                Some(match (kind, target) {
                    ("workspace", _) | (_, None) => format!("[link to {shown}]"),
                    (_, Some(t)) => format!("[link to {shown}: {t}]"),
                })
            }
            "breadcrumb" => Some("[breadcrumb]".to_string()),
            "table_of_contents" => Some("[table of contents]".to_string()),
            // Итог, заметки и расшифровка читаются отдельно и подвешиваются
            // детьми. Метка добавляет состояние: у идущей расшифровки под ней
            // пока пусто, и это не то же самое, что сказать нечего.
            "meeting_notes" | "transcription" => {
                let status = payload(b).and_then(|p| p.get("status")).and_then(Value::as_str);
                let title = rich_text(payload(b).and_then(|p| p.get("title")));
                let shown = match status {
                    Some(s) if s != "notes_ready" => format!("{title} ({s})").trim().to_string(),
                    _ => title,
                };
                Some(labelled(ty, &shown))
            }
            "unsupported" => Some(format!("[unsupported block: {}]",
                payload(b).and_then(|p| p.get("block_type")).and_then(Value::as_str).unwrap_or("?"))),
            // Структурные контейнеры своего текста не несут; метка для них
            // добавила бы строку на колонку впустую.
            "column_list" | "column" | "table" => None,
            "tab" => Some("[tab]".to_string()),
            "synced_block" => {
                let is_copy = payload(b).and_then(|p| p.get("synced_from")).is_some_and(|v| !v.is_null());
                let has_children = b.get("children").and_then(Value::as_array).is_some_and(|c| !c.is_empty());
                if is_copy { Some("[synced block]".to_string()) }
                else if has_children { None }
                else { Some("[synced block: empty]".to_string()) }
            }
            "" => Some("[unreadable block]".to_string()),
            // Тип, о котором эта версия не слышала, называется, а не
            // выбрасывается: печатать ничего — это ровно то, с чего началась
            // первая жалоба, и следующий тип блока вернул бы её обратно.
            _ => Some(if text.is_empty() { labelled(ty, "") } else { text }),
        };

        if let Some(o) = out.as_ref() {
            lines.push(if indent.is_empty() { o.clone() } else { prefix_lines(o, indent) });
        }

        let child_indent = match (&out, ty) {
            (None, _) => indent.to_string(),
            (_, "quote") => format!("{indent}> "),
            _ => format!("{indent}  "),
        };
        if let Some(children) = b.get("children").and_then(Value::as_array) {
            if !children.is_empty() {
                let nested = blocks_to_text(children, &child_indent);
                if !nested.is_empty() { lines.push(nested); }
            }
        }
        if let Some(reason) = b.get("elided").and_then(Value::as_str) {
            lines.push(format!("{child_indent}[nested content: {}]", elision_reason(reason)));
        }
    }

    lines.join("\n")
}
