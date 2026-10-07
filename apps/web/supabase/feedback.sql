-- LogRoom feedback table + RLS policy.
--
-- Usage:
--   1. Create a Supabase project (https://supabase.com) — reuse the same
--      project as waitlist.sql if you already have one.
--   2. Open the SQL Editor and run this file once.
--   3. Copy the Project URL and anon public key from Settings > API into
--      PUBLIC_SUPABASE_URL / PUBLIC_SUPABASE_ANON_KEY (see .env.example / README.md).
--   4. Check submissions in the Supabase Table Editor (public.feedback) — there is
--      no read API exposed to the browser (insert-only, see policy below).

create table public.feedback (
  id bigint generated always as identity primary key,
  message text not null,
  email text,
  locale text,
  created_at timestamptz not null default now()
);

alter table public.feedback enable row level security;

-- Insert-only: the anon key (shipped in the browser bundle) may only INSERT.
-- No SELECT/UPDATE/DELETE policy exists, so submitted feedback can't be read,
-- changed, or removed via the public API — only from the Supabase dashboard.
create policy "Allow anon insert" on public.feedback
  for insert
  to anon
  with check (true);
