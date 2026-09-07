//! Условия отбора тикетов — одно место на все запросы.
//!
//! Список и обход отбирают одинаково, и это не совпадение, а требование: если
//! `walk` понимает «тег infra» иначе, чем `ls`, человек проверяет не то, что
//! видел в списке, и узнаёт об этом в лучшем случае случайно. Два набора
//! условий в двух местах расходятся не «если», а «когда».

use serde::Deserialize;

#[derive(Deserialize, Default, Clone)]
pub struct Filters {
    pub status: Option<String>,
    /// Теги через запятую. Все перечисленные должны быть на тикете.
    pub tag: Option<String>,
    /// Тег совпадает целиком, а не входит частью.
    #[serde(default)]
    pub strict: bool,
    pub title: Option<String>,
    pub assignee: Option<String>,
    pub project: Option<String>,
    pub module: Option<String>,
    /// Тикеты всех, а не только свои. Проигрывает явно названному исполнителю.
    #[serde(default)]
    pub all: bool,
}

/// Готовые значения для подстановки. Держатся отдельно от строки запроса,
/// потому что аргументы обязаны пережить её использование.
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
    /// Кого считать исполнителем: явно названного, «всех» или спрашивающего.
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

    /// Человекочитаемое описание отбора. По нему же различаются обходы: назвали
    /// другое — начался другой обход.
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

/// Дописывает условия к запросу и собирает аргументы.
///
/// Условия именно дописываются, а не прячутся за `($1 is null or …)`: такая
/// запись короче, но ОТКЛЮЧАЕТ индексы — планировщик не может доказать, что
/// частичный индекс подойдёт, когда условие спрятано за «или». Замерено на 5340
/// тикетах: последовательный скан и 4179 прочитанных строк на каждый список.
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
            // Массив и `@>`, а не `= any(…)`: первое берёт GIN-индекс.
            sql.push_str(&format!(" and tags @> ${}::text[]", args.len()));
        }
        (Some(list), false) => {
            // Теги живут семействами: `infra` и `initiative:infra-…` про
            // одно и то же. Отсюда вхождение по умолчанию — и скан вместо
            // индекса как честная его цена.
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
