-- Indexing and refusing are one switch today. They should not be.
--
-- Turning vectorisation on for a real workspace does two things at once: it
-- indexes everything, and it starts REFUSING creates that look like duplicates.
-- The refusal threshold was measured on thirty tickets. On thousands, the top
-- of the non-duplicate range rises — template-generated tickets crowd it — and
-- the first thing anyone would see is a wave of false stops on ordinary work.
--
-- So the stop is its own flag, off by default. Index first, watch the scores
-- the refusal path logs, calibrate on the real corpus, then switch it on.
-- min_score sits here too: the number belongs to the workspace, not to the
-- binary, and moving it must not need a release.
alter table vector_policy
  add column if not exists stop_on_similar boolean not null default false;
alter table vector_policy
  add column if not exists min_score double precision not null default 0.75;

alter table vector_policy drop constraint if exists vector_policy_min_score_check;
alter table vector_policy
  add constraint vector_policy_min_score_check check (min_score > 0 and min_score <= 1);
