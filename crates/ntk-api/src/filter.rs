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
}

/// The bound values. Kept apart from the query string because the arguments
/// have to outlive its use.
pub struct Bound {
    pub status: Option<String>,
    pub tags: Option<Vec<String>>,
    pub title: Option<String>,
    pub assignee: Option<String>,
    pub project: Option<String>,
    pub module: Option<String>,
    pub strict: bool,
}

impl Filters {
    /// Who counts as the assignee: the one named, everyone, or the caller.
    pub fn bind(&self, me: &str) -> Bound {
        let tags: Option<Vec<String>> = self.tag.as_deref().map(|t| {
            t.split(',').map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect()
        });
        Bound {
            status: self.status.clone(),
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
        }
    }

    /// A human-readable description of the filter. Walks are told apart by it
    /// too: name something else and a different walk begins.
    pub fn describe(&self) -> String {
        let mut p = Vec::new();
        if let Some(v) = &self.status { p.push(format!("статус {v}")); }
        if let Some(v) = &self.assignee { p.push(format!("исполнитель {v}")); }
        if let Some(v) = &self.project { p.push(format!("проект {v}")); }
        if let Some(v) = &self.module { p.push(format!("модуль {v}")); }
        if let Some(v) = &self.tag { p.push(format!("тег {v}{}", if self.strict { " целиком" } else { "" })); }
        if let Some(v) = &self.title { p.push(format!("заголовок «{v}»")); }
        if self.all { p.push("все, не только свои".into()); }
        if p.is_empty() { "без отбора".into() } else { p.join(", ") }
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
    if b.status.is_some() {
        args.push(&b.status);
        sql.push_str(&format!(" and status = ${}", args.len()));
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
}
