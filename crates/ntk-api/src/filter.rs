//! Ticket filtering — one place for every query.
//!
//! The list and the walk filter identically, and that is a requirement rather
//! than a coincidence: if `walk` understood "tag infra" differently from `ls`,
//! a person would be reviewing something other than what they saw in the list,
//! and would find out by luck at best. Two sets of conditions in two places
//! drift apart not "if" but "when".

use serde::Deserialize;

#[derive(Deserialize, Default, Clone)]
pub struct Filters {
    pub status: Option<String>,
    /// Comma-separated tags. Every one listed must be on the ticket.
    pub tag: Option<String>,
    /// The tag matches in full rather than as a part.
    #[serde(default)]
    pub strict: bool,
    pub title: Option<String>,
    pub assignee: Option<String>,
    pub project: Option<String>,
    pub module: Option<String>,
    /// Everyone's tickets, not just yours. Loses to an explicitly named assignee.
    #[serde(default)]
    pub all: bool,
    /// Отбор по датам: `created_at:gte:2026-09-01,created_at:lte:2026-09-12`.
    /// Через запятую, сколько угодно выражений и по разным полям сразу.
    pub date: Option<String>,
}

/// Одно условие по дате: колонка, сторона, значение.
#[derive(Clone)]
pub struct DateTerm {
    pub col: &'static str,
    /// true — «не раньше» (>=), false — «не позже» (<=). Обе включающие.
    pub gte: bool,
    pub value: String,
    /// Названо со временем — сравниваем момент, а не день.
    pub with_time: bool,
}

/// Поля, по которым можно отбирать. Список закрытый: имя колонки уходит в SQL
/// напрямую, и открытым его делать нельзя ни при каких удобствах.
const DATE_FIELDS: &[(&str, &str)] = &[
    ("created_at", "created_at"),
    ("updated_at", "updated_at"),
    ("started_at", "started_at"),
    ("closed_at", "closed_at"),
    ("due", "due"),
];

/// У `due` времени нет: это date, а не момент. Остальные пять — timestamptz.
const DATE_ONLY: &[&str] = &["due"];

/// Разбор выражений. Ошибку возвращаем, а не глотаем: молча пропущенное
/// условие означает выдачу шире запрошенной, и заметить это неоткуда.
pub fn parse_dates(spec: Option<&str>) -> Result<Vec<DateTerm>, String> {
    let Some(spec) = spec else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for raw in spec.split(',').map(str::trim).filter(|x| !x.is_empty()) {
        // По ПЕРВЫМ ДВУМ двоеточиям, а не по всем: время само полно двоеточий,
        // и `created_at:lte:2026-12-07T12:35:00` иначе распалось бы на пять
        // кусков вместо трёх. Так значение остаётся обычным ISO.
        let parts: Vec<&str> = raw.splitn(3, ':').collect();
        if parts.len() != 3 {
            return Err(format!(
                "date filter \"{raw}\" is not field:op:date — for example created_at:gte:2026-09-01"
            ));
        }
        let (field, op, value) = (parts[0], parts[1], parts[2]);
        let Some((_, col)) = DATE_FIELDS.iter().find(|(name, _)| *name == field) else {
            return Err(format!(
                "unknown date field \"{field}\". The ones there are: {}",
                DATE_FIELDS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
            ));
        };
        let gte = match op {
            "gte" => true,
            "lte" => false,
            other => {
                return Err(format!(
                    "unknown date operator \"{other}\": gte or lte, and both include the day named"
                ))
            }
        };
        // Форма проверяется здесь, а не в базе: иначе кривая дата приходит
        // отказом Postgres, из которого не следует, ЧТО именно написано не так.
        //
        // Время отделяется по ISO: `T`, а хвостовой `Z` — обозначение UTC, и
        // сервер в UTC и живёт, так что он ничего не меняет.
        let value = value.strip_suffix('Z').unwrap_or(value);
        let (day, time) = match value.split_once(['T', ' ']) {
            Some((d, t)) => (d, Some(t)),
            None => (value, None),
        };
        let day_ok = day.len() == 10
            && day.as_bytes()[4] == b'-'
            && day.as_bytes()[7] == b'-'
            && day.bytes().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
        if !day_ok {
            return Err(format!("date \"{value}\" is not YYYY-MM-DD"));
        }
        if let Some(t) = time {
            if DATE_ONLY.contains(&field) {
                return Err(format!(
                    "{field} holds a date, not a moment: drop the time from \"{value}\""
                ));
            }
            let parts: Vec<&str> = t.split(':').collect();
            let shape_ok = (parts.len() == 2 || parts.len() == 3)
                && parts.iter().all(|p| p.len() == 2 && p.bytes().all(|c| c.is_ascii_digit()));
            if !shape_ok {
                return Err(format!("time \"{t}\" is not HH:MM or HH:MM:SS"));
            }
        }
        // Время отдаём Postgres в его обычном виде; сервер живёт в UTC, и
        // названный момент понимается как UTC.
        let stored = match time {
            None => day.to_string(),
            // Явный часовой пояс, а не надежда на настройку сеанса: без него
            // «12:35» значило бы разное на разных серверах.
            Some(t) => format!("{day} {t}+00"),
        };
        out.push(DateTerm { col, gte, value: stored, with_time: time.is_some() });
    }
    Ok(out)
}

/// Условия по датам в SQL. Сравнение приводится к ДАТЕ, а не к моменту:
/// `updated_at <= '2026-09-12'` на метке времени отсекло бы всё, что случилось
/// в этот день после полуночи, и «по двенадцатое включительно» означало бы
/// «по одиннадцатое». Обе границы включающие.
pub fn apply_dates<'a>(
    sql: &mut String,
    args: &mut Vec<&'a (dyn tokio_postgres::types::ToSql + Sync)>,
    terms: &'a [DateTerm],
) {
    for t in terms {
        args.push(&t.value);
        let op = if t.gte { ">=" } else { "<=" };
        if t.with_time {
            sql.push_str(&format!(" and {} {} ${}::timestamptz", t.col, op, args.len()));
        } else {
            sql.push_str(&format!(" and {}::date {} ${}::date", t.col, op, args.len()));
        }
    }
}

/// The bound values. Kept apart from the query string because the arguments
/// have to outlive its use.
pub struct Bound {
    /// Статусы списком, а не одним значением.
    ///
    /// В `ls` многостатусный отбор был сделан мимо этого модуля, своим куском
    /// кода, — то есть путей отбора стало два, ровно то, от чего этот модуль и
    /// заводился. Список живёт здесь, и `find` получает его тем же способом,
    /// что список и обход.
    pub statuses: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    pub title: Option<String>,
    pub assignee: Option<String>,
    pub project: Option<String>,
    pub module: Option<String>,
    pub strict: bool,
    pub dates: Vec<DateTerm>,
}

impl Filters {
    /// Who counts as the assignee: the one named, everyone, or the caller.
    pub fn bind(&self, me: &str) -> Result<Bound, String> {
        let tags: Option<Vec<String>> = self.tag.as_deref().map(|t| {
            t.split(',').map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect()
        });
        let statuses: Option<Vec<String>> = self.status.as_deref().map(|t| {
            t.split(',').map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect()
        });
        Ok(Bound {
            dates: parse_dates(self.date.as_deref())?,
            statuses: statuses.filter(|v: &Vec<String>| !v.is_empty()),
            tags: tags.filter(|v: &Vec<String>| !v.is_empty()),
            title: self.title.clone(),
            assignee: match (&self.assignee, self.all) {
                (Some(a), _) => Some(a.clone()),
                (None, true) => None,
                (None, false) => Some(me.to_string()),
            },
            project: self.project.clone(),
            module: self.module.clone(),
            strict: self.strict,
        })
    }

    /// A human-readable description of the filter. Walks are told apart by it
    /// too: name something else and a different walk begins.
    pub fn describe(&self) -> String {
        let mut p = Vec::new();
        if let Some(v) = &self.status { p.push(format!("status {v}")); }
        if let Some(v) = &self.assignee { p.push(format!("assignee {v}")); }
        if let Some(v) = &self.project { p.push(format!("project {v}")); }
        if let Some(v) = &self.module { p.push(format!("module {v}")); }
        if let Some(v) = &self.tag { p.push(format!("tag {v}{}", if self.strict { " exactly" } else { "" })); }
        if let Some(v) = &self.title { p.push(format!("title {v:?}")); }
        if let Some(v) = &self.date { p.push(format!("date {v}")); }
        if self.all { p.push("everyone's, not just mine".into()); }
        if p.is_empty() { "no filter".into() } else { p.join(", ") }
    }
}

/// Appends the conditions to the query and collects the arguments.
///
/// The conditions are appended rather than hidden behind `($1 is null or …)`:
/// that form is shorter but it DISABLES indexes — the planner cannot prove a
/// partial index applies when the condition sits behind an "or". Measured on
/// 5340 tickets: a sequential scan and 4179 rows read for every listing.
pub fn apply<'a>(
    sql: &mut String,
    args: &mut Vec<&'a (dyn tokio_postgres::types::ToSql + Sync)>,
    b: &'a Bound,
) {
    if b.statuses.is_some() {
        // `= any(...)` вместо цепочки «или»: одно условие, и частичный индекс
        // по статусу остаётся применимым.
        args.push(&b.statuses);
        sql.push_str(&format!(" and status = any(${})", args.len()));
    }
    if b.assignee.is_some() {
        args.push(&b.assignee);
        sql.push_str(&format!(" and assignee = ${}", args.len()));
    }
    if b.project.is_some() {
        args.push(&b.project);
        sql.push_str(&format!(" and project_id = ${}", args.len()));
    }
    if b.module.is_some() {
        args.push(&b.module);
        sql.push_str(&format!(" and module = ${}", args.len()));
    }
    match (&b.tags, b.strict) {
        (Some(_), true) => {
            args.push(&b.tags);
            // An array with `@>` rather than `= any(…)`: the first takes the GIN index.
            sql.push_str(&format!(" and tags @> ${}::text[]", args.len()));
        }
        (Some(list), false) => {
            // Tags come in families: `infra` and `initiative:infra-…` are
            // about the same thing. Hence substring matching by default — and
            // a scan instead of an index as its honest price.
            for t in list {
                args.push(t);
                sql.push_str(&format!(
                    " and exists (select 1 from unnest(tags) x where x ilike '%' || ${} || '%')",
                    args.len()
                ));
            }
        }
        (None, _) => {}
    }
    if b.title.is_some() {
        args.push(&b.title);
        sql.push_str(&format!(" and title ilike '%' || ${} || '%'", args.len()));
    }
    apply_dates(sql, args, &b.dates);
}

#[cfg(test)]
mod date_tests {
    use super::parse_dates;

    #[test]
    fn both_bounds_and_several_fields_in_one_call() {
        let t = parse_dates(Some("created_at:gte:2026-09-01,closed_at:lte:2026-09-12")).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!((t[0].col, t[0].gte, t[0].with_time), ("created_at", true, false));
        assert_eq!((t[1].col, t[1].gte, t[1].with_time), ("closed_at", false, false));
    }

    /// Время полно двоеточий, а двоеточие уже делит выражение. Резать надо по
    /// первым двум — иначе обычная запись ISO разваливается.
    #[test]
    fn a_moment_survives_its_own_colons() {
        let t = parse_dates(Some("created_at:lte:2026-12-07T12:35:00")).unwrap();
        assert_eq!(t.len(), 1);
        assert!(t[0].with_time);
        assert_eq!(t[0].value, "2026-12-07 12:35:00+00");
    }

    #[test]
    fn a_trailing_zulu_is_accepted_and_means_utc() {
        let t = parse_dates(Some("updated_at:gte:2026-12-07T12:35Z")).unwrap();
        assert_eq!(t[0].value, "2026-12-07 12:35+00");
    }

    /// Отказ, а не пропуск: молча выброшенное условие расширяет выдачу, и
    /// узнать об этом неоткуда.
    #[test]
    fn nonsense_is_refused_rather_than_dropped() {
        for bad in [
            "banana:gte:2026-09-01",
            "created_at:near:2026-09-01",
            "created_at:gte:07.12.2026",
            "created_at:gte",
            "due:gte:2026-09-01T10:00",
        ] {
            assert!(parse_dates(Some(bad)).is_err(), "{bad} should have been refused");
        }
    }

    #[test]
    fn nothing_asked_nothing_added() {
        assert!(parse_dates(None).unwrap().is_empty());
        assert!(parse_dates(Some("")).unwrap().is_empty());
    }
}
