-- One number decided two different things, and they are not the same decision.
--
-- min_score governed both what the duplicate search RETURNS and what creating
-- REFUSES on. The PM asked to lower it to 0.60 so a reworded duplicate (0.639)
-- and a same-file duplicate (0.749) stop being cut from the answer. The
-- reasoning behind it is sound and worth quoting: an agent reads the list and
-- discards what does not fit, so a false hit costs one glance.
--
-- That reasoning holds for a LIST. It does not hold for a REFUSAL. Measured on
-- 40 recent tickets against the live corpus, each as its own query:
--   0.80 -> 1 of 40 refused (2%)      0.70 -> 11 of 40 (28%)
--   0.77 -> 3 of 40 (8%)              0.65 -> 25 of 40 (62%)
--   0.75 -> 4 of 40 (10%)             0.60 -> 36 of 40 (90%)
-- At 0.60 nine creations in ten would be refused. The cost of a false refusal
-- is not a glance — it is a second call carrying skip_search, every time, and a
-- flag set out of routine stops meaning anything at all.
--
-- So the number is split in two. What is SHOWN keeps the low bar: nothing is
-- cut, exactly as asked. What BLOCKS keeps a high one: only near-copies.
-- Between them, creating succeeds AND carries the list — the agent still sees
-- what it needs to see.
alter table vector_policy
  add column if not exists block_score double precision not null default 0.80;

alter table vector_policy drop constraint if exists vector_policy_block_score_check;
alter table vector_policy
  add constraint vector_policy_block_score_check check (block_score > 0 and block_score <= 1);
