-- Один выпуск на платформу, а не один на версию.
--
-- Первичным ключом была версия, поэтому 0.5.2 могла существовать ровно для
-- одной платформы: попытка положить сборку под Linux рядом с macOS молча
-- отбрасывалась через on conflict do nothing, и Linux-клиентам обновляться
-- было нечем — при том что сервер бодро отдавал им macOS-бинарь.

do $$
begin
  if exists (select 1 from pg_constraint where conname = 'releases_pkey'
               and conrelid = 'core.releases'::regclass
               and array_length(conkey, 1) = 1) then
    alter table core.releases drop constraint releases_pkey;
    alter table core.releases add primary key (version, platform);
  end if;
end $$;
