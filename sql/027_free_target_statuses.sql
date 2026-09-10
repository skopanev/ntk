-- Moving your own started work forward should not need force.
--
-- The guard exists so nobody edits SOMEONE ELSE'S work mid-flight. Finishing
-- what you started is not an edit — it is the ordinary path every ticket takes.
-- Closing was already exempt; `to_test` was not, so handing work to a tester
-- demanded `--force` as routine. A flag set out of routine stops meaning
-- anything, which is exactly what the guard is insured against.
--
-- The exemption stays NARROW and is now data, not code: it applies only when
-- the status is the single field changing, and only for statuses marked here.
-- Closing while rewriting someone else's body still needs force.
alter table statuses add column if not exists free_target boolean not null default false;

-- Terminal statuses: closing was already exempt in code, now it is stated here.
update statuses set free_target = true where grp = 'complete';
-- Handing work on for checking is the same kind of move.
update statuses set free_target = true where name = 'to_test';
