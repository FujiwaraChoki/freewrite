-- Freewrite cloud storage.
--
-- Every entry is one row, keyed by (user_id, id). `id` is the same UUID that
-- appears in the local filename `[uuid]-[yyyy-MM-dd-HH-mm-ss].md`, so an entry
-- keeps its identity on every device. Deletions are soft (`deleted = true`) so
-- other devices learn about them on their next sync.

create table if not exists public.entries (
    user_id           uuid        not null default auth.uid() references auth.users (id) on delete cascade,
    id                uuid        not null,
    filename          text        not null,
    content           text        not null default '',
    deleted           boolean     not null default false,
    client_updated_at timestamptz not null default now(),
    created_at        timestamptz not null default now(),
    -- Set by the server on every write; clients use it as their pull cursor.
    updated_at        timestamptz not null default now(),
    primary key (user_id, id)
);

create index if not exists entries_user_updated_at_idx on public.entries (user_id, updated_at);

create or replace function public.entries_set_updated_at()
returns trigger
language plpgsql
as $$
begin
    new.updated_at = now();
    return new;
end;
$$;

drop trigger if exists entries_set_updated_at on public.entries;
create trigger entries_set_updated_at
    before insert or update on public.entries
    for each row execute function public.entries_set_updated_at();

alter table public.entries enable row level security;

drop policy if exists "Users can read their own entries" on public.entries;
create policy "Users can read their own entries"
    on public.entries for select
    to authenticated
    using ((select auth.uid()) = user_id);

drop policy if exists "Users can insert their own entries" on public.entries;
create policy "Users can insert their own entries"
    on public.entries for insert
    to authenticated
    with check ((select auth.uid()) = user_id);

drop policy if exists "Users can update their own entries" on public.entries;
create policy "Users can update their own entries"
    on public.entries for update
    to authenticated
    using ((select auth.uid()) = user_id)
    with check ((select auth.uid()) = user_id);

drop policy if exists "Users can delete their own entries" on public.entries;
create policy "Users can delete their own entries"
    on public.entries for delete
    to authenticated
    using ((select auth.uid()) = user_id);
