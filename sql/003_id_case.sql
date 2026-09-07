-- Идентификаторы в Notion оказались не только строчными: в базе живут
-- BQ_Analitycs-kvtzwecu5j и подобные. Проверка формата их отвергала.
--
-- Нормализовать id нельзя: он вшит в сообщения коммитов, его набирают руками
-- и ищут по префиксу. Приводить к нижнему регистру значило бы разорвать эти
-- ссылки ради красоты колонки.
alter table tickets drop constraint if exists tickets_id_check;
alter table tickets add constraint tickets_id_check
  check (id ~ '^[A-Za-z0-9_-]+-[A-Za-z0-9]{4,}$');

-- Раз регистр в данных разный, поиск обязан быть регистронезависимым: CLI
-- сегодня приводит введённое к нижнему регистру и находит тикет, а
-- регистрозависимое сравнение в Postgres молча вернуло бы «нет такого».
create index if not exists tickets_id_lower on tickets (lower(id) text_pattern_ops);
