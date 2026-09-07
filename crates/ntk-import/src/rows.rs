//! Разбор страницы Notion в строку таблицы.
//!
//! Единственное место, где типы свойств Notion превращаются в колонки. После
//! U14 этот файл умрёт вместе с Notion, поэтому он намеренно отделён от всего
//! остального.

use serde_json::Value;

pub fn text_prop(props: &Value, name: &str) -> Option<String> {
    let p = props.get(name)?;
    let joined: String = match p.get("type")?.as_str()? {
        "title" => collect(p.get("title")),
        "rich_text" => collect(p.get("rich_text")),
        _ => return None,
    };
    if joined.is_empty() { None } else { Some(joined) }
}

fn collect(v: Option<&Value>) -> String {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|r| {
                    r.get("plain_text").and_then(Value::as_str)
                        .or_else(|| r.pointer("/text/content").and_then(Value::as_str))
                        .unwrap_or("")
                })
                .collect::<String>()
        })
        .unwrap_or_default()
}

pub fn select_prop(props: &Value, name: &str) -> Option<String> {
    props.pointer(&format!("/{name}/select/name")).and_then(Value::as_str).map(str::to_string)
}

pub fn status_prop(props: &Value, name: &str) -> Option<String> {
    props.pointer(&format!("/{name}/status/name")).and_then(Value::as_str).map(str::to_string)
}

pub fn multi_select(props: &Value, name: &str) -> Vec<String> {
    props
        .pointer(&format!("/{name}/multi_select"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_string)).collect())
        .unwrap_or_default()
}

pub fn date_prop(props: &Value, name: &str) -> Option<String> {
    props.pointer(&format!("/{name}/date/start")).and_then(Value::as_str).map(str::to_string)
}

pub fn people_ids(props: &Value, name: &str) -> Vec<String> {
    props
        .pointer(&format!("/{name}/people"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|p| p.get("id").and_then(Value::as_str).map(str::to_string)).collect())
        .unwrap_or_default()
}

pub fn relation_ids(props: &Value, name: &str) -> Vec<String> {
    props
        .pointer(&format!("/{name}/relation"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|r| r.get("id").and_then(Value::as_str).map(str::to_string)).collect())
        .unwrap_or_default()
}

/// Сведение двух шкал приоритетов к одной.
///
/// В базе живут high/med/low (585 тикетов) и P0–P3 вместе с normal, medium и
/// голой цифрой 4 (33 тикета). Отображение НЕ выдумано на месте: оно
/// перечислено здесь целиком, чтобы верификатор мог показать, что во что
/// превратилось, а решение при желании пересмотрели по списку, а не по коду.
pub fn canonical_priority(raw: Option<&str>) -> Option<&'static str> {
    match raw?.trim().to_ascii_lowercase().as_str() {
        "high" | "p0" | "p1" => Some("high"),
        "med" | "medium" | "normal" | "p2" => Some("med"),
        "low" | "p3" | "4" => Some("low"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_scales_collapse_and_nothing_is_invented() {
        // Каждое значение, реально встреченное в снимке, обязано отобразиться:
        // молча пропавший приоритет — это тикет, потерявший важность.
        for raw in ["high", "P0", "P1", "med", "medium", "normal", "P2", "low", "P3", "4"] {
            assert!(canonical_priority(Some(raw)).is_some(), "{raw} не отобразился");
        }
        assert_eq!(canonical_priority(Some("P0")), Some("high"));
        assert_eq!(canonical_priority(Some("P3")), Some("low"));
        assert_eq!(canonical_priority(None), None);
        // Незнакомое значение не превращается в med «на всякий случай»:
        // импорт обязан о нём споткнуться.
        assert_eq!(canonical_priority(Some("срочно")), None);
    }
}
