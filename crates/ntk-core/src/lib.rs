//! Модель тикета — одна на клиент и на сервис.
//!
//! Она здесь именно затем, чтобы API и CLI не завели каждый свою: форма
//! ответа — это контракт, на который уже опираются агенты, и разъехавшись
//! однажды, эти два описания разъедутся навсегда.
//!
//! Два правила про пустоту достались от Notion-версии и переносятся как есть,
//! потому что читатели на них уже полагаются: `tags` присутствует ВСЕГДА и
//! всегда список, а `deps` при пустоте ОТСУТСТВУЕТ. Асимметрия намеренная:
//! на первом варианте падал парсер после `--set-tags ""`, на втором держится
//! отличие «зависимостей нет» от «поле не запрашивали».

/// Каталог инструментов: одно описание на терминал, локальный MCP и MCP по HTTP.
pub mod tools;

use serde::{Deserialize, Serialize};

/// Группа статусов. В Notion это свойство типа `status`, в Postgres — колонка
/// в справочнике `statuses`: своего понятия групп там нет, а гард «тикет уже
/// подобран» держится именно на них.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusGroup {
    Todo,
    InProgress,
    Complete,
}

impl StatusGroup {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "todo" => Some(Self::Todo),
            "in_progress" => Some(Self::InProgress),
            "complete" => Some(Self::Complete),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::InProgress => "in_progress",
            Self::Complete => "complete",
        }
    }
}

/// Статус из справочника воркспейса. Набор — свойство базы, а не константа в
/// коде: он разный у воркспейсов и меняется без релиза.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub name: String,
    pub group: StatusGroup,
    pub sort: i16,
}

/// Тикет в том виде, в каком его отдают наружу. Порядок полей — порядок в
/// JSON; он повторяет сегодняшний вывод, чтобы diff читался глазами.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    // Ни одно поле не выбрасывается из ответа при пустом значении.
    //
    // Раньше пустые пропускались, и форма ответа зависела от данных: у тикета
    // в работе появлялся started_at, у закрытого — closed_at, у остальных их не
    // было вовсе. Читающий не может отличить «значения нет» от «поля не
    // существует» и вынужден гадать; хуже того, гадать приходится по одной
    // выдаче, а поведение меняется от тикета к тикету.
    //
    // Цена — несколько лишних `null` на тикет. Она известна и мала: на пятистах
    // тикетах это десятки килобайт против трёх мегабайт самой выдачи.
    /// Короткий ключ вида `proj-xxxxxxxxxx`. Его придумывает клиент, а не
    /// база: он вшит в сообщения коммитов, его набирают руками и ищут по
    /// префиксу, поэтому uuid первичным ключом быть не может.
    pub id: String,
    /// Обещан в описании инструмента `ntk_ls` («carries each ticket's own
    /// uuid»), поэтому переносится, хотя первичным ключом не является.
    pub uuid: String,
    pub title: String,
    pub status: String,
    pub priority: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub assignee: Option<String>,
    pub project: Option<String>,
    /// Модуль — единица работы внутри проекта. Пусто, пока не назначен.
    pub module: Option<String>,
    /// Всегда присутствует, пустой список включительно.
    pub tags: Vec<String>,
    #[serde(default)]
    pub deps: Vec<String>,
    pub due: Option<String>,
    pub body: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub started_at: Option<String>,
    /// Момент перехода в терминальный статус — единственный источник правды о
    /// том, когда работа закончилась: время создания и правки на этот вопрос
    /// не отвечают.
    pub closed_at: Option<String>,
}

/// Разбор идентификатора так, как его набирают люди: `proj-1a2b3c4d5e`
/// целиком или один хвост `1a2b3c4d5e`, который дополняется проектом.
pub fn spellings(partial: &str, project: Option<&str>) -> Vec<String> {
    let want = partial.trim().to_ascii_lowercase();
    if want.is_empty() {
        return Vec::new();
    }
    match project {
        Some(p) if !p.is_empty() => {
            let prefix = format!("{}-", p.to_ascii_lowercase());
            if want.starts_with(&prefix) {
                vec![want]
            } else {
                let full = format!("{prefix}{want}");
                vec![want, full]
            }
        }
        _ => vec![want],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_hash_and_full_id_are_the_same_ticket() {
        assert_eq!(spellings("1a2b3c", Some("proj")), vec!["1a2b3c", "proj-1a2b3c"]);
        assert_eq!(spellings("proj-1a2b3c", Some("proj")), vec!["proj-1a2b3c"]);
        assert_eq!(spellings("proj-1a2b3c", None), vec!["proj-1a2b3c"]);
        assert!(spellings("  ", Some("proj")).is_empty());
    }

    #[test]
    fn every_field_is_present_even_when_empty() {
        // Раньше здесь закреплялась асимметрия: tags выводились всегда, а deps
        // при пустоте пропадали. Она защищала отличие «зависимостей нет» от
        // «поле не запрашивали» — но с тех пор deps заполняются всегда, и
        // различать стало нечего, а форма ответа, зависящая от данных, гонит
        // читателя гадать по одной выдаче.
        //
        // Тест переписан вслед за решением, а не подогнан: проверяется, что
        // ПУСТОЙ тикет отдаёт все поля, включая null.
        let t = Ticket {
            id: "tst-1".into(), uuid: "u".into(), title: "t".into(), status: "open".into(),
            priority: None, kind: None, assignee: None, project: None, module: None,
            tags: vec![], deps: vec![], due: None, body: None,
            created_at: "now".into(), updated_at: "now".into(),
            started_at: None, closed_at: None,
        };
        let json = serde_json::to_string(&t).unwrap();
        for field in [
            "id", "uuid", "title", "status", "priority", "type", "assignee",
            "project", "module", "tags", "deps", "due", "body", "created_at", "updated_at",
            "started_at", "closed_at",
        ] {
            assert!(
                json.contains(&format!("\"{field}\"")),
                "поле {field} пропало из ответа: форма не должна зависеть от данных — {json}"
            );
        }
        assert!(json.contains(r#""tags":[]"#));
        assert!(json.contains(r#""deps":[]"#));
    }

    #[test]
    fn status_groups_round_trip() {
        for name in ["todo", "in_progress", "complete"] {
            assert_eq!(StatusGroup::parse(name).unwrap().as_str(), name);
        }
        assert!(StatusGroup::parse("review").is_none());
    }
}
