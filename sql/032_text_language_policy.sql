-- Tickets in one language, by the owner's decision.
--
-- Bodies and titles are mixed Russian and English today. That hurts reading,
-- and it hurts deduplication in a way no threshold can fix: the same work
-- described in two languages does not meet by similarity. Measured: 2280 of
-- 4185 live tickets carry Cyrillic, but only 293 of the 1526 filed in the last
-- week — the move to English is already happening on its own, so a gate now
-- stops roughly one new ticket in five rather than most of them.
--
-- Three states, and the owner picks per workspace with an UPDATE:
--   any   — nothing is checked;
--   warn  — the ticket is written and the answer carries a warning;
--   latin — writing is refused.
-- Default is `warn`: a rule that starts by refusing teaches people to route
-- around it before they learn what it wants.
--
-- What is actually detected is CYRILLIC, not "non-English", and the column is
-- named for what it does. Detecting a language is a guess; detecting an
-- alphabet is a fact, and a rule must not claim more than it checks.
create table if not exists text_policy (
  only_row boolean primary key default true check (only_row),
  language text not null default 'warn' check (language in ('any', 'warn', 'latin'))
);
insert into text_policy (only_row) values (true) on conflict do nothing;
