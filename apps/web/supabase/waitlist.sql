-- LogRoom waitlist table + RLS policy.
--
-- Usage:
--   1. Create a Supabase project (https://supabase.com).
--   2. Open the SQL Editor and run this file once.
--   3. Copy the Project URL and anon public key from Settings > API into
--      PUBLIC_SUPABASE_URL / PUBLIC_SUPABASE_ANON_KEY (see .env.example / README.md).
--   4. Check submissions in the Supabase Table Editor (public.waitlist) — there is
--      no read API exposed to the browser (insert-only, see policy below).

create table public.waitlist (
  id bigint generated always as identity primary key,
  email text not null unique check (email ~* '^[^@\s]+@[^@\s]+\.[^@\s]+$'),
  locale text,
  created_at timestamptz not null default now()
);

alter table public.waitlist enable row level security;

-- Insert-only: the anon key (shipped in the browser bundle) may only INSERT.
-- No SELECT/UPDATE/DELETE policy exists, so submitted emails can't be read,
-- changed, or removed via the public API — only from the Supabase dashboard.
create policy "Allow anon insert" on public.waitlist
  for insert
  to anon
  with check (true);
