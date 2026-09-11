-- The embedding input was clipped at 2000 — shorter than a legal ticket.
--
-- The write limits are per FIELD: body 2000, title 256. The embedding input is
-- their join, title + newline + body, so a legal ticket reaches 2257. Clipping
-- at 2000 cut the tail off the body of any ticket with a long title, even
-- though the whole ticket was within limits.
--
-- Measured on 4164 live tickets before changing it: 431 were being clipped, by
-- 64 characters on average and 213 at worst; at 2257 not one is — the longest
-- legal ticket joins to 2213.
--
-- Done now on purpose: nothing is indexed in that workspace yet, so the change
-- costs nothing. After indexing it would mean re-embedding every ticket, since
-- the fingerprint covers the clipped text and reconcile compares input_max.
--
-- The number must equal EMBED_INPUT_MAX in the service. Divergence does not
-- corrupt anything, but it makes every fingerprint disagree, and reconcile
-- would re-index the whole workspace every six hours, for money. The canary in
-- reconcile shouts when more than half the points look stale — that is what it
-- is for.
create or replace function vector_input_sha(title text, body text) returns text
language sql immutable as $$
  select encode(sha256(convert_to(left($1 || chr(10) || $2, 2257), 'UTF8')), 'hex')
$$;
