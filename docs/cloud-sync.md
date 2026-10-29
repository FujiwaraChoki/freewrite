# Cloud sync

Freewrite keeps writing local-first: every keystroke is saved to a plain
markdown file on disk, exactly as before. Signing in to an account adds a
background sync that mirrors those files to a [Supabase](https://supabase.com)
project, so the same entries show up on macOS, Windows and Linux.

## Setting up the backend

1. Create a Supabase project.
2. Run [`supabase/migrations/20261029000000_entries.sql`](../supabase/migrations/20261029000000_entries.sql)
   in the SQL editor (or `supabase db push` with the Supabase CLI).
3. Under **Authentication → Sign In / Providers**, keep **Email** enabled.
   Turn **Confirm email** off if you want accounts to work immediately after
   sign-up; leave it on to require a confirmation link first.
4. Copy the project URL and the **publishable** (formerly "anon") key from
   **Project Settings → API**. They are safe to ship inside the app: every
   row is protected by row-level security and only readable by its owner.

Each app reads them from:

| App | Where |
| --- | --- |
| macOS | `SupabaseURL` / `SupabaseAnonKey` in [`freewrite/Cloud.plist`](../freewrite/Cloud.plist) (bundled with the app), overridden by the `FREEWRITE_SUPABASE_URL` / `FREEWRITE_SUPABASE_ANON_KEY` environment variables (e.g. in the Xcode scheme, for development). Xcode's generated Info.plist can't carry custom keys, but the same two keys are also read from Info.plist if the project switches to its own. |
| Windows / Linux | `FREEWRITE_SUPABASE_URL` / `FREEWRITE_SUPABASE_ANON_KEY` at build time (baked in) or run time, or `cloud.json` in the config directory |

When neither is set the apps work exactly as before, fully offline, and the
account section explains that sync isn't configured.

## Data model

One row per entry in `public.entries`, primary key `(user_id, id)`:

| Column | Meaning |
| --- | --- |
| `id` | The UUID from the filename `[uuid]-[yyyy-MM-dd-HH-mm-ss].md` |
| `filename` | The full local filename, so every device writes the same file |
| `content` | The full file contents (including the leading `\n\n`) |
| `deleted` | Tombstone; the entry was deleted on some device |
| `client_updated_at` | When the writing device last modified the file |
| `updated_at` | Set by a trigger on every write; the pull cursor |

## Sync algorithm

Both clients implement the same algorithm. State is kept per user in
`sync-state-<user_id>.json`:

```json
{ "cursor": "2026-10-29T12:00:00.000000+00:00",
  "entries": { "<uuid>": { "filename": "...", "hash": "<sha256 of content>" } } }
```

`hash` is the SHA-256 (hex) of the content as of the last successful sync.
An entry is **locally dirty** when its file's hash differs from the recorded
hash (or it has no record yet).

Each sync run:

1. **Pull.** Fetch rows with `updated_at > cursor - 60s` ordered by
   `updated_at, id` (paged, 500 per page). The 60 second overlap guards
   against rows that commit out of order; re-processing a row is harmless.
   For each row:
   - `deleted`: if the local file is unchanged since the last sync, delete it
     and drop the record. If it was edited locally, keep it; the push step
     will resurrect it.
   - Local content already equal to the remote content: just record the hash.
   - No local file: if there's a record whose hash equals the remote hash,
     the user deleted it locally and the push step will send a tombstone.
     Otherwise write the remote file.
   - Local file not dirty: overwrite it with the remote content.
   - Both changed (conflict): keep the local file in place and save the
     remote version as a new entry with a fresh UUID and the same date stamp,
     so no writing is ever lost. Record the remote hash for the original so
     the local version is pushed.

   Remote writes to a local file are conditional: the file is only replaced if
   it still has the content the decision was based on, so a keystroke saved
   mid-sync is never overwritten.
2. **Push.** Upsert (`on_conflict=user_id,id`, `Prefer: resolution=merge-duplicates`)
   every locally dirty entry, in batches of 50. Brand-new entries that are
   empty or still the untouched welcome guide are skipped so every device
   doesn't upload its own blank page. Records whose file no longer exists
   are pushed as tombstones (`deleted = true`, empty content) and removed.
3. Save the new cursor (the largest `updated_at` seen while pulling).

Sync runs after sign-in, every 30 seconds while signed in, when switching
entries, and when the app closes.

## Auth

Email + password through Supabase Auth's REST API:

- `POST /auth/v1/signup` – create an account (may require email confirmation)
- `POST /auth/v1/token?grant_type=password` – sign in
- `POST /auth/v1/token?grant_type=refresh_token` – refresh the session
- `POST /auth/v1/logout` – sign out

The session is stored in the OS credential store where one is available
(macOS Keychain, Windows Credential Manager, the Secret Service on Linux),
falling back to a `session.json` readable only by the current user.
Signing out stops syncing but leaves local files untouched.
