//! The tool catalogue: ONE description for every surface.
//!
//! There are three of them and they all talk to the same people and agents:
//! the `ntk` commands in the terminal, the local MCP server inside the client,
//! and MCP over HTTP inside the service. Each kept its own text, and a comment
//! above one of them claimed they "match the local server word for word". They
//! did not: the client served sixteen tools, HTTP served thirteen — the three
//! module tools were missing entirely — and close, deps, rm, meta and walk had
//! drifted apart in wording. A model used to one surface had to relearn the
//! other, part of the work was simply unreachable over HTTP, and nothing could
//! have told us.
//!
//! So this is not a copy kept for convenience, it is the source: the surfaces
//! render their descriptions from here and have none of their own. That they
//! agree is checked by a test, not promised in a comment.
//!
//! Everything a caller reads is in English: models work with these texts, and
//! the language of a hint must not depend on which surface is plugged in.
//!
//! The write limits are spelled out as digits, because a description is a
//! literal and there is nothing to interpolate with. A test keeps the digit in
//! the text from drifting away from the digit in the code: it looks for exactly
//! `TITLE_MAX` and `BODY_MAX`. The real limit lives in the `write_limits` table
//! and is enforced by the database; what is here is what we say about it.

use serde_json::{json, Map, Value};

/// Title limit, in characters.
pub const TITLE_MAX: usize = 256;
/// Body limit, in characters.
pub const BODY_MAX: usize = 2000;

/// Field type in the call schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    Str,
    Int,
    Num,
    Bool,
    StrList,
}

#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub name: &'static str,
    pub ty: Ty,
    pub required: bool,
    pub desc: &'static str,
    /// Length limit. Goes into the schema and into the field's own text.
    pub max_len: Option<usize>,
}

impl Field {
    const fn req(name: &'static str, ty: Ty, desc: &'static str) -> Self {
        Self { name, ty, required: true, desc, max_len: None }
    }
    const fn opt(name: &'static str, ty: Ty, desc: &'static str) -> Self {
        Self { name, ty, required: false, desc, max_len: None }
    }
    const fn capped(mut self, n: usize) -> Self {
        self.max_len = Some(n);
        self
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Tool {
    /// The MCP name: `ntk_create`.
    pub name: &'static str,
    /// The terminal command: `create`. Empty when there is no command.
    pub cli: &'static str,
    /// Human-readable name for `annotations.title`.
    pub title: &'static str,
    /// One line: `ntk --help` and short hints.
    pub about: &'static str,
    /// The full description: MCP and `ntk <cmd> --help`.
    pub desc: &'static str,
    pub read_only: bool,
    pub destructive: bool,
    pub idempotent: bool,
    pub fields: &'static [Field],
}

impl Tool {
    /// The call schema for MCP.
    pub fn input_schema(&self) -> Value {
        let mut props = Map::new();
        let mut required = Vec::new();
        for f in self.fields {
            let mut p = Map::new();
            let ty = match f.ty {
                Ty::Str => "string",
                Ty::Int => "integer",
                Ty::Num => "number",
                Ty::Bool => "boolean",
                Ty::StrList => "array",
            };
            p.insert("type".into(), json!(ty));
            if f.ty == Ty::StrList {
                p.insert("items".into(), json!({"type":"string"}));
            }
            if let Some(n) = f.max_len {
                p.insert("maxLength".into(), json!(n));
            }
            if !f.desc.is_empty() {
                p.insert("description".into(), json!(f.desc));
            }
            props.insert(f.name.into(), Value::Object(p));
            if f.required {
                required.push(json!(f.name));
            }
        }
        let mut s = Map::new();
        s.insert("type".into(), json!("object"));
        s.insert("properties".into(), Value::Object(props));
        // We omit an empty `required`: its absence reads as "nothing is
        // mandatory", while an empty list reads to some clients as a broken
        // schema.
        if !required.is_empty() {
            s.insert("required".into(), Value::Array(required));
        }
        Value::Object(s)
    }

    /// MCP hints: does it read, write, or overwrite.
    ///
    /// They do not remove the permission prompt — that decision belongs to the
    /// client on a person's machine — but a reading tool stops looking like a
    /// writing one.
    pub fn annotations(&self) -> Value {
        let mut a = Map::new();
        a.insert("title".into(), json!(self.title));
        a.insert("readOnlyHint".into(), json!(self.read_only));
        a.insert("openWorldHint".into(), json!(false));
        if !self.read_only {
            a.insert("destructiveHint".into(), json!(self.destructive));
            a.insert("idempotentHint".into(), json!(self.idempotent));
        }
        Value::Object(a)
    }

    /// The full `tools/list` entry.
    pub fn manifest(&self) -> Value {
        json!({
            "name": self.name,
            "annotations": self.annotations(),
            "description": self.desc,
            "inputSchema": self.input_schema(),
        })
    }

    pub fn field(&self, name: &str) -> Option<&'static Field> {
        self.fields.iter().find(|f| f.name == name)
    }
}

pub fn get(name: &str) -> Option<&'static Tool> {
    ALL.iter().find(|t| t.name == name)
}

/// Look a tool up by its terminal command.
pub fn by_cli(cli: &str) -> Option<&'static Tool> {
    ALL.iter().find(|t| !t.cli.is_empty() && t.cli == cli)
}

/// The whole list for `tools/list`.
pub fn manifest() -> Value {
    Value::Array(ALL.iter().map(Tool::manifest).collect())
}

// --- shared fields ----------------------------------------------------------

const WS: Field =
    Field::req("workspace", Ty::Str, "Workspace. Required: there is no default.");
const WS_OPT: Field = Field::opt(
    "workspace",
    Ty::Str,
    "Workspace. Taken from .ntkrc when omitted; nothing is guessed.",
);
const ID: Field =
    Field::req("id", Ty::Str, "Identifier, proj-xxxxxxxxxx. Case does not matter.");
const FORCE: Field = Field::opt(
    "force",
    Ty::Bool,
    "Lift the \"already picked up\" guard. Set it deliberately: you are stepping into someone else's work.",
);
const TAG: Field = Field::opt(
    "tag",
    Ty::Str,
    "Comma-separated tags: \"alpha,ios\". Every tag listed must be on the ticket — AND, not OR. Matches by SUBSTRING by default: \"infra\" also finds \"initiative:infra\".",
);
const STRICT: Field =
    Field::opt("strict", Ty::Bool, "The tag must match in full, not as a part.");
const Q_TITLE: Field =
    Field::opt("title", Ty::Str, "Filter by title: substring, case-insensitive.");
const ASSIGNEE_PICK: Field = Field::opt(
    "assignee",
    Ty::Str,
    "Filter by assignee. Overrides the \"mine\" default.",
);
const PROJECT_PICK: Field = Field::opt("project", Ty::Str, "Filter by project.");
const ALL_ONES: Field =
    Field::opt("all", Ty::Bool, "Show everyone's tickets, not just yours.");
const MODULE_PICK: Field = Field::opt(
    "module",
    Ty::Str,
    "Filter by module — the unit of work inside a project.",
);
const STATUS_PICK: Field = Field::opt(
    "status",
    Ty::Str,
    "Filter by status: open, in_progress, to_test, to_review, reviewed, blocked, done.",
);
const PRIORITY: Field =
    Field::opt("priority", Ty::Str, "Priority. ntk_meta lists the ones this workspace has.");
const KIND: Field =
    Field::opt("type", Ty::Str, "Ticket type. ntk_meta lists the ones this workspace has.");
const ASSIGNEE_SET: Field = Field::opt(
    "assignee",
    Ty::Str,
    "Who the ticket is for: the short identifier from the people directory, e.g. sk.",
);

pub const ALL: &[Tool] = &[
    Tool {
        name: "ntk_whoami",
        cli: "whoami",
        title: "Who am I",
        about: "Who you are and which workspaces you can reach.",
        desc: "Who you are and which workspaces you can reach. Call this FIRST if you do not know which workspace to pass: every other tool requires it and there is no default.",
        read_only: true,
        destructive: false,
        idempotent: true,
        fields: &[],
    },
    Tool {
        name: "ntk_ls",
        cli: "ls",
        title: "List tickets",
        about: "List tickets. Yours only by default.",
        desc: "List the workspace's tickets. Yours only by default; all=true shows everyone's. Paged: limit and offset.",
        read_only: true,
        destructive: false,
        idempotent: true,
        fields: &[
            WS,
            STATUS_PICK,
            Field::opt("limit", Ty::Int, "50 by default, 500 at most."),
            Field::opt("offset", Ty::Int, "Skip the first N — the next page."),
            ALL_ONES,
            TAG,
            STRICT,
            Q_TITLE,
            ASSIGNEE_PICK,
            PROJECT_PICK,
            MODULE_PICK,
            Field::opt("stale", Ty::Int, "Only tickets that have been sitting in their CURRENT status longer than this many days. Answers the one question nothing could answer before: which work was abandoned — a container died, an agent never came back, and the ticket stayed in in_progress. Counted from the moment of the last status change, not from the last edit: fixing a typo yesterday must not make a ticket look freshly taken."),
            Field::opt("count", Ty::Bool, "Return only the NUMBER of matches. The 500-row output cap does not limit the count: the database counts it."),
        ],
    },
    Tool {
        name: "ntk_walk",
        cli: "walk",
        title: "Walk tickets for review",
        about: "Walk tickets one by one for review. Changes nothing.",
        desc: "Walk tickets one by one for review: returns the next one not yet shown under this filter and changes NOTHING on the ticket. Not to be confused with ntk_next, which takes a ticket INTO WORK — walking thirty tickets that way would assign all thirty to you, wrecking the queue. Invent walk_id once and pass the same one at every step; separate sessions walk independently.",
        read_only: false,
        destructive: false,
        idempotent: false,
        fields: &[
            WS,
            Field::req("walk_id", Ty::Str, "Identifier of this walk. Invent it ONCE and pass the same one at every step — the server remembers what it has already shown for it. Separate sessions walk independently, so two agents do not disturb each other."),
            STATUS_PICK,
            TAG,
            STRICT,
            Q_TITLE,
            ASSIGNEE_PICK,
            PROJECT_PICK,
            MODULE_PICK,
            ALL_ONES,
            Field::opt("reset", Ty::Bool, "Forget what was shown and start over."),
        ],
    },
    Tool {
        name: "ntk_show",
        cli: "show",
        title: "Show ticket",
        about: "Show the whole ticket: fields, body, dependencies.",
        desc: "The whole ticket: fields, body, dependencies, who holds it and what it waits for.",
        read_only: true,
        destructive: false,
        idempotent: true,
        fields: &[WS, ID],
    },
    Tool {
        name: "ntk_next",
        cli: "next",
        title: "Take a free ticket",
        about: "Take the next free ticket into work.",
        desc: "Take the next free ticket into work. The claim is atomic: one ticket will not go to two agents. The queue raises blockers of started work by itself — do not assemble that choice by hand out of ntk_ls and ntk_deps.",
        read_only: false,
        destructive: false,
        idempotent: false,
        fields: &[
            WS,
            Field::opt("prefer", Ty::Str, "Tags in order of preference, comma-separated. An ORDER, not a filter: if nothing matches them, any suitable ticket is taken. The preference ranks BELOW blockers — urgency first (its own or inherited from whatever the ticket unblocks), then \"unblocks started work\", and only then these tags."),
            Field::opt("tag", Ty::Str, "Comma-separated tags. Unlike prefer this EXCLUDES: no match, no ticket."),
            STRICT,
            PROJECT_PICK,
            Field::opt("module", Ty::Str, "Filter by one named module."),
            Field::opt("has_module", Ty::Bool, "Any LIVE module instead of a named one: \"a unit of work is assigned\". An archived module does not count."),
            ASSIGNEE_PICK,
            Field::opt("dry_run", Ty::Bool, "Show what WOULD be taken and do NOT take it. Same filter, same order. The answer is advice, not a reservation: by the time you claim it the ticket may be gone."),
        ],
    },
    Tool {
        name: "ntk_start",
        cli: "start",
        title: "Take a ticket into work",
        about: "Take a named ticket into work.",
        desc: "Take a NAMED ticket into work. If someone already took it, you get a refusal carrying the current status, not silence.",
        read_only: false,
        destructive: false,
        idempotent: false,
        fields: &[WS, ID],
    },
    Tool {
        name: "ntk_create",
        cli: "create",
        title: "Create ticket",
        about: "Create a ticket.",
        desc: "Create a ticket. The server assigns the identifier. Where vectorisation is switched on, this first looks for tickets that already say the same thing and REFUSES if it finds one, listing what it found; pass skip_search=true to file anyway. LIMITS: title 256 characters, body 2000. Overflow is refused WHOLE, never truncated: split it into several tickets, write tighter, or carry the bulk in an attachment. Write tickets in English.",
        read_only: false,
        destructive: false,
        idempotent: false,
        fields: &[
            WS,
            Field::req("project", Ty::Str, "Project — the prefix of the ticket identifier."),
            Field::req("title", Ty::Str, "Title, at most 256 characters. English.").capped(TITLE_MAX),
            Field::opt("body", Ty::Str, "Body in markdown, at most 2000 characters. English.").capped(BODY_MAX),
            ASSIGNEE_SET,
            Field::opt("tags", Ty::StrList, "Tags, unsigned here: creating sets the whole set at once."),
            Field::opt("module", Ty::Str, "Module — the unit of work inside a project. ntk_meta lists the allowed ones; do not guess."),
            PRIORITY,
            Field::opt("status", Ty::Str, "Starting status. Without it the server takes the first one in the todo group."),
            KIND,
            Field::opt("deps", Ty::StrList, "Identifiers of the tickets this one waits for."),
            Field::opt("skip_search", Ty::Bool, "File the ticket without looking for existing ones that already say the same thing. Where vectorisation is switched on, creating searches first and REFUSES if it finds a likely duplicate, listing what it found; set this to file anyway. Read the list before you set it — the point of the stop is that the work may already be in the queue."),
        ],
    },
    Tool {
        name: "ntk_update",
        cli: "update",
        title: "Change ticket",
        about: "Change a ticket: status, title, body, assignee, tags.",
        desc: "Change a ticket in ONE call: status, title, body, assignee and tags together, applied in one transaction — the ticket is never seen half-changed. A ticket outside the todo group is already picked up by someone and needs force. LIMITS: title 256 characters, body 2000. Overflow is refused WHOLE, never truncated: split it into several tickets, write tighter, or carry the bulk in an attachment. Write tickets in English.",
        read_only: false,
        destructive: true,
        idempotent: true,
        fields: &[
            WS,
            ID,
            Field::opt("status", Ty::Str, "New status. Changing a ticket outside the todo group needs force."),
            Field::opt("title", Ty::Str, "Title, at most 256 characters. English.").capped(TITLE_MAX),
            Field::opt("body", Ty::Str, "The whole body, at most 2000 characters. REPLACES the previous one: to add a line use body_append. English.").capped(BODY_MAX),
            Field::opt("body_append", Ty::Str, "Append to the end of the body without touching what is written — the two are separated by a BLANK LINE, so the addition reads as its own paragraph rather than running into the last sentence. Not accepted together with body; body REPLACES, this one adds. The 2000 limit counts the RESULT of the join, separator included: appending into a full ticket is refused, not silently truncated."),
            ASSIGNEE_SET,
            Field::opt("tag_edits", Ty::StrList, "Every edit carries a sign: [\"+alpha\",\"-legacy\"]. The sign is required, otherwise \"add\" will one day turn out to be \"replace everything\"."),
            Field::opt("dep_edits", Ty::StrList, "Dependency edits, each with a sign: [\"+proj-abc\",\"-proj-xyz\"]. Plus starts waiting for that ticket, minus stops. The sign is required for the same reason as on tags: a bare list would one day mean \"replace them all\" and links would vanish silently. The target must exist — an edge to nowhere turns \"waiting for that ticket\" into \"waiting for nothing\", and nobody finds out."),
            Field::opt("dep_set", Ty::StrList, "REPLACE the whole dependency set with the tickets listed; an empty list clears every one. Separate from dep_edits on purpose: there a sign is required, here it must be absent — two intentions in one field once cost us six lost tags."),
            Field::opt("module", Ty::Str, "Module. An empty string clears it. When changing project, a module of the new project is required."),
            Field::opt("project", Ty::Str, "Move the ticket to another project. A module of the new project then becomes required."),
            PRIORITY,
            KIND,
            Field::opt("due", Ty::Str, "Due date, YYYY-MM-DD. An empty string clears it — that is \"drop the deadline\", not \"leave it alone\"."),
            FORCE,
        ],
    },
    Tool {
        name: "ntk_close",
        cli: "close",
        title: "Close ticket",
        about: "Close a ticket: move it to done.",
        desc: "Close a ticket — move it to done. The closing date is set by the transition; it cannot be set by hand. Describe what was done via ntk_update BEFORE closing: without that the close is pointless, because the next reader will not know how it ended.",
        read_only: false,
        destructive: false,
        idempotent: true,
        fields: &[WS, ID, FORCE],
    },
    Tool {
        name: "ntk_tag",
        cli: "tag",
        title: "Edit tags",
        about: "Edit a ticket's tags.",
        desc: "Tags only, nothing else. If you are changing anything besides tags, use ntk_update — it does everything in one call. Every edit carries a sign: [\"+alpha\",\"-legacy\"]. The sign is required.",
        read_only: false,
        destructive: true,
        idempotent: true,
        fields: &[
            WS,
            ID,
            Field::req("edits", Ty::StrList, "Tag edits, each with a sign: [\"+alpha\",\"-legacy\"]. A tag without a sign is refused."),
            FORCE,
        ],
    },
    Tool {
        name: "ntk_deps",
        cli: "deps",
        title: "Dependencies",
        about: "A ticket's dependencies: what it stands on and what stands on it.",
        desc: "A ticket's dependencies: what it stands on and what stands on it.",
        read_only: true,
        destructive: false,
        idempotent: true,
        fields: &[WS, ID],
    },
    Tool {
        name: "ntk_rm",
        cli: "rm",
        title: "Remove ticket",
        about: "Remove a ticket.",
        desc: "Remove a ticket: it stops showing up but is not erased. A real delete would quietly free everyone who was waiting on it.",
        read_only: false,
        destructive: true,
        idempotent: true,
        fields: &[WS, ID],
    },
    Tool {
        name: "ntk_meta",
        cli: "meta",
        title: "Workspace directories",
        about: "What the workspace has: statuses, priorities, projects, people.",
        desc: "What the workspace has: statuses and their groups, priorities, projects, people. Replaces the former schema, users, projects and workspaces calls.",
        read_only: true,
        destructive: false,
        idempotent: true,
        fields: &[WS],
    },
    Tool {
        name: "ntk_find",
        cli: "find",
        title: "Find similar tickets",
        about: "Find tickets that already say the same thing.",
        desc: "Find tickets whose text is close to the one given, so the same work is not filed twice. Searches EVERY status and EVERYONE's tickets by default, closed ones included; narrow it with status, tag, assignee, project or module — the same filters ntk_ls takes, understood the same way. Only available where vectorisation is switched on; without it the answer is a refusal, not an empty list — an empty list would read as \"nothing like it exists\". Every hit is checked against the database before it is returned, so a ticket that was removed or rewritten cannot come back through a stale vector. Ranked by closeness, closest first.",
        read_only: true,
        destructive: false,
        idempotent: true,
        fields: &[
            WS,
            Field::opt("text", Ty::Str, "The text to look for: a title, a sentence, or the whole ticket you are about to file. Only the first 2257 characters take part — exactly what a legal ticket joins to.").capped(BODY_MAX),
            Field::opt("body", Ty::Str, "More text, joined to `text`. Convenient when the title and the body are already separate.").capped(BODY_MAX),
            Field::opt("id", Ty::Str, "Look for tickets similar to THIS existing one, instead of passing text. Not accepted together with text or body."),
            Field::opt("status", Ty::Str, "Comma-separated statuses to look in: \"open,in_progress\". By DEFAULT every status is searched, closed ones included — a duplicate of something already done is the most useful thing this can tell you. Narrow it when you only care about live work."),
            TAG,
            STRICT,
            Field::opt("assignee", Ty::Str, "Only this person's tickets. By default everyone's are searched: a duplicate is filed over somebody else's ticket more often than over your own."),
            PROJECT_PICK,
            MODULE_PICK,
            Field::opt("limit", Ty::Int, "How many to return. 10 by default, 20 at most."),
            Field::opt("min_score", Ty::Num, "Closeness cut-off between 0 and 1. Omit it and the workspace policy decides — there is no fixed number here, because the right one depends on the corpus. Measured on a live workspace of 4179 tickets: lowering the bar does NOT simply find more duplicates, because the distributions overlap — a reworded duplicate scored 0.639 while unrelated work in the same area scored 0.677. Pass a low value to see the tail and judge for yourself; pass a high one to see near-copies only."),
        ],
    },
    Tool {
        name: "ntk_modules",
        cli: "modules",
        title: "Project modules",
        about: "Project modules.",
        desc: "Project modules. Shows both live and archived ones: an archived module stays visible but cannot be chosen for new work.",
        read_only: true,
        destructive: false,
        idempotent: true,
        fields: &[WS, Field::opt("project", Ty::Str, "Only this project's modules. Without it — all of them.")],
    },
    Tool {
        name: "ntk_modules_add",
        cli: "modules-add",
        title: "Add modules",
        about: "Add project modules without touching the rest of the registry.",
        desc: "Add modules to a project WITHOUT touching the rest of the registry: nothing leaves the live set. Take this instead of replace when you only need to add names.",
        read_only: false,
        destructive: false,
        idempotent: true,
        fields: &[
            WS,
            Field::req("project", Ty::Str, "Project."),
            Field::req("add", Ty::StrList, "Names of the modules to add."),
        ],
    },
    Tool {
        name: "ntk_modules_replace",
        cli: "modules-replace",
        title: "Replace modules",
        about: "Replace a project's module list as a whole.",
        desc: "Replace a project's module list as a whole. The list is taken as COMPLETE: a module missing from it leaves the live set — into the archive if tickets reference it, and for good if none do. An archived module that returns to the list becomes live again. To drop one module, send all the others, not that one. An empty list is refused: it would wipe the project's whole registry.",
        read_only: false,
        destructive: true,
        idempotent: true,
        fields: &[
            WS,
            Field::req("project", Ty::Str, "Project."),
            Field::req("modules", Ty::StrList, "The COMPLETE list of the project's live modules."),
        ],
    },
];

/// The workspace field for terminal commands: there it comes from `.ntkrc`.
pub const CLI_WORKSPACE: &Field = &WS_OPT;

/// Short description for `ntk --help`.
///
/// Panics on an unknown name on purpose: these names are literals in the
/// argument parser, so a typo is a build error in spirit, not an empty hint in
/// somebody's terminal. The parser is built at startup, so it fails on the very
/// first run rather than in the middle of someone's work.
pub fn about(name: &str) -> &'static str {
    get(name).unwrap_or_else(|| panic!("{name} нет в каталоге инструментов")).about
}

/// Full description for `ntk <command> --help`.
pub fn desc(name: &str) -> &'static str {
    get(name).unwrap_or_else(|| panic!("{name} нет в каталоге инструментов")).desc
}

/// Help text for one argument, for `ntk <command> --help`.
///
/// Panics on an unknown name on purpose: these names are literals in the
/// argument parser, so a typo is a build error in spirit, not an empty hint in
/// somebody's terminal. The parser is built at startup, so it fails on the very
/// first run rather than in the middle of someone's work.
pub fn arg(tool: &str, field: &str) -> &'static str {
    get(tool)
        .unwrap_or_else(|| panic!("no tool {tool} in the catalogue"))
        .field(field)
        .unwrap_or_else(|| panic!("no field {tool}.{field} in the catalogue"))
        .desc
}

/// Help for the `modules` command: in the terminal three tools are folded into
/// one command with flags, so the text is joined from three descriptions.
pub fn modules_help() -> String {
    format!(
        "{}\n\n--add: {}\n\n--replace: {}",
        desc("ntk_modules"),
        desc("ntk_modules_add"),
        desc("ntk_modules_replace")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_is_named_and_described() {
        for t in ALL {
            assert!(t.name.starts_with("ntk_"), "{}", t.name);
            assert!(!t.title.is_empty(), "{}", t.name);
            assert!(!t.about.is_empty(), "{}", t.name);
            assert!(!t.desc.is_empty(), "{}", t.name);
            assert!(!t.about.contains('\n'), "{}: the short description must be one line", t.name);
            assert!(
                t.about.chars().count() <= 80,
                "{}: the short description will not fit the --help column ({})",
                t.name,
                t.about.chars().count()
            );
        }
    }

    /// Models read these texts, and the language of a hint must not depend on
    /// the surface. Cyrillic here is a trace of a description written past the
    /// catalogue.
    #[test]
    fn everything_the_caller_reads_is_english() {
        let cyrillic = |s: &str| s.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c));
        for t in ALL {
            assert!(!cyrillic(t.title), "{}: title is in Russian", t.name);
            assert!(!cyrillic(t.about), "{}: about is in Russian", t.name);
            assert!(!cyrillic(t.desc), "{}: desc is in Russian", t.name);
            for f in t.fields {
                assert!(!cyrillic(f.desc), "{}.{}: description is in Russian", t.name, f.name);
            }
        }
        assert!(!cyrillic(CLI_WORKSPACE.desc));
    }

    /// A field with no description is an unnamed parameter: the client sees a
    /// name and guesses what belongs in it. Not one may be empty.
    #[test]
    fn every_argument_says_what_it_is() {
        for t in ALL {
            for f in t.fields {
                assert!(!f.desc.trim().is_empty(), "{}.{}: has no description", t.name, f.name);
            }
        }
    }

    #[test]
    fn names_and_cli_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for t in ALL {
            assert!(seen.insert(t.name), "{} listed twice", t.name);
        }
        let mut cli = std::collections::HashSet::new();
        for t in ALL.iter().filter(|t| !t.cli.is_empty()) {
            assert!(cli.insert(t.cli), "{} listed twice", t.cli);
        }
    }

    #[test]
    fn fields_are_unique_within_a_tool() {
        for t in ALL {
            let mut seen = std::collections::HashSet::new();
            for f in t.fields {
                assert!(seen.insert(f.name), "{}: field {} listed twice", t.name, f.name);
            }
        }
    }

    /// The digit in the text and the digit in the code must be one digit.
    ///
    /// A description is a constant string with nothing to interpolate, so the
    /// limit inside it is written by hand. This test is the only thing standing
    /// between changing the constant and forgetting the text: the tool would
    /// promise one number while the database demanded another, and the only way
    /// to find out would be a refusal on write.
    #[test]
    fn stated_limits_match_the_constants() {
        let title = TITLE_MAX.to_string();
        let body = BODY_MAX.to_string();
        for name in ["ntk_create", "ntk_update"] {
            let t = get(name).unwrap();
            assert!(t.desc.contains(&title), "{name}: the description omits the title limit");
            assert!(t.desc.contains(&body), "{name}: the description omits the body limit");
            for f in ["title", "body"] {
                let fd = t.field(f).unwrap();
                let want = if f == "title" { &title } else { &body };
                assert!(fd.desc.contains(want.as_str()), "{name}.{f}: the limit is not stated");
                assert_eq!(
                    fd.max_len,
                    Some(if f == "title" { TITLE_MAX } else { BODY_MAX }),
                    "{name}.{f}: the limit is missing from the schema"
                );
            }
        }
        // Appending counts the joined result — that is stated too.
        let app = get("ntk_update").unwrap().field("body_append").unwrap();
        assert!(app.desc.contains(&body), "body_append: the limit is not stated");
    }

    #[test]
    fn schema_marks_required_fields_and_types() {
        let c = get("ntk_create").unwrap().input_schema();
        let req = c["required"].as_array().unwrap();
        assert!(req.iter().any(|v| v == "workspace"));
        assert!(req.iter().any(|v| v == "title"));
        assert!(!req.iter().any(|v| v == "body"), "a body is never mandatory");
        assert_eq!(c["properties"]["deps"]["type"], "array");
        assert_eq!(c["properties"]["deps"]["items"]["type"], "string");
        assert_eq!(c["properties"]["title"]["maxLength"], 256);

        // A tool with no fields must carry no `required` at all.
        let w = get("ntk_whoami").unwrap().input_schema();
        assert!(w.get("required").is_none());
    }

    #[test]
    fn read_only_tools_carry_no_write_hints() {
        for t in ALL.iter().filter(|t| t.read_only) {
            let a = t.annotations();
            assert!(a.get("destructiveHint").is_none(), "{}", t.name);
            assert_eq!(a["readOnlyHint"], true, "{}", t.name);
        }
    }
}
