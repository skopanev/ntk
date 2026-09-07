-- Реестр модулей проекта.
--
-- Модуль — единица работы внутри проекта: у одного проекта их много, и тикет
-- принадлежит ровно одному. Что считать модулем, NTK не знает и знать не
-- должен: реестр наполняется снаружи полным списком, а здесь только хранится
-- и проверяется.
--
-- Ключ составной, и это главное в схеме. Имя модуля уникально лишь внутри
-- проекта: один и тот же `lib` может существовать в двух проектах и означать
-- разное. Внешний ключ тикета идёт на ПАРУ, поэтому сослаться на модуль
-- чужого проекта нельзя структурно — база откажет, а не проверяющий вспомнит.
create table if not exists modules (
  project_id  text not null references projects(id) on delete cascade,
  name        text not null,
  created_at  timestamptz not null default now(),
  primary key (project_id, name)
);

alter table tickets add column if not exists module text;

-- Внешний ключ на пару. Он же запрещает поставить модуль тикету без проекта:
-- пара (null, 'lib') не совпадёт ни с одной строкой реестра.
alter table tickets drop constraint if exists tickets_module_fk;
alter table tickets add constraint tickets_module_fk
  foreign key (project_id, module) references modules(project_id, name);

create index if not exists tickets_module on tickets (project_id, module)
  where module is not null;

-- Требование заполненности по тегу.
--
-- Механизм общий, словарь — воркспейса: какой тег обязывает назвать модуль,
-- решает тот, кто ведёт конвейер, и меняет это одной строкой, без релиза. Так
-- же устроен гард «тикет уже подобран»: политика живёт в данных.
create table if not exists tag_requirements (
  tag             text primary key,
  requires_module boolean not null default true
);
