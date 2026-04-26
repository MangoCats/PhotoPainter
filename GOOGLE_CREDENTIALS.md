# Google Calendar Credentials Setup

The code in `server/src/gcal_creds.rs` needs four values:
`CLIENT_ID`, `CLIENT_SECRET`, `REFRESH_TOKEN`, and `CALENDAR_IDS`.

Refresh tokens expire after **7 days** when the OAuth consent screen is in *Testing* mode.
Publishing the app (no Google review required for personal use) makes them permanent.

---

## Part A — Fix expiring tokens on an existing setup

If you already have a working `CLIENT_ID` and `CLIENT_SECRET` but the refresh token
keeps expiring, the cause is that the OAuth consent screen is still in *Testing* mode.

### A1 — Publish the OAuth consent screen

1. Go to <https://console.cloud.google.com> and open your project.
2. Navigate to **APIs & Services → OAuth consent screen**.
3. Click **Publish App**.  
   Google will warn that the app is "unverified" — that is fine for personal use.  
   Click **Confirm**.  
   The status changes from *Testing* to *In production*.

### A2 — Re-run the auth flow to get a permanent refresh token

Publishing does not automatically refresh existing tokens.
Run the OAuth flow once more (Part B, Step 4 below) using your existing
`CLIENT_ID` and `CLIENT_SECRET` to obtain a new `REFRESH_TOKEN` that will not expire.

---

## Part B — Full setup from scratch

### B1 — Create a Google Cloud project and enable the Calendar API

1. Go to <https://console.cloud.google.com>.
2. Click the project drop-down at the top → **New Project**.  
   Name it (e.g. `PhotoPainter`), click **Create**.
3. With the new project selected, go to **APIs & Services → Library**.
4. Search for **Google Calendar API**, click it, then click **Enable**.

### B2 — Configure the OAuth consent screen

1. Go to **APIs & Services → OAuth consent screen**.
2. Choose **External**, click **Create**.
3. Fill in the required fields:
   - App name: `PhotoPainter` (or anything)
   - User support email: your Gmail address
   - Developer contact: your Gmail address
4. Click **Save and Continue**.
5. On the **Scopes** page, click **Add or Remove Scopes**.  
   Search for `calendar.readonly` and select  
   `https://www.googleapis.com/auth/calendar.readonly`.  
   Click **Update**, then **Save and Continue**.
6. On the **Test users** page, add your Gmail address, click **Save and Continue**.
7. Review the summary and click **Back to Dashboard**.
8. **Click "Publish App"** — this is what makes refresh tokens permanent.  
   Confirm when prompted.

### B3 — Create OAuth credentials (Desktop application)

1. Go to **APIs & Services → Credentials**.
2. Click **Create Credentials → OAuth client ID**.
3. Choose **Desktop app** as the application type.
4. Name it (e.g. `PhotoPainter desktop`), click **Create**.
5. Click **Download JSON** on the confirmation dialog.  
   Save the file as `client_secret.json` somewhere convenient (not in the repo).
6. Note the **Client ID** and **Client Secret** shown on screen — these go into `gcal_creds.rs`.

### B4 — Run the one-time OAuth flow to get a refresh token

This requires Python 3 and one package.  Open a terminal:

```bash
pip install google-auth-oauthlib
```

Save the following as `get_token.py` in the same folder as `client_secret.json`:

```python
from google_auth_oauthlib.flow import InstalledAppFlow

SCOPES = ['https://www.googleapis.com/auth/calendar.readonly']

flow = InstalledAppFlow.from_client_secrets_file('client_secret.json', SCOPES)
creds = flow.run_local_server(port=0)

print()
print('CLIENT_ID:    ', creds.client_id)
print('CLIENT_SECRET:', creds.client_secret)
print('REFRESH_TOKEN:', creds.refresh_token)
```

Run it:

```bash
python get_token.py
```

A browser window opens automatically.  Log in with your Google account, click through
the "unverified app" warning (click **Advanced → Go to PhotoPainter (unsafe)**), and
grant calendar read access.  The terminal prints your three credential values.

> **Note:** the "unverified" warning appears only during this one-time setup flow.
> It does not affect normal operation of the server.

### B5 — Find your calendar IDs

Each calendar you want to include needs its ID.

**Primary calendar:** your Gmail address (e.g. `you@gmail.com`).

**Other calendars:**
1. Open Google Calendar in a browser.
2. In the left sidebar, hover over a calendar name and click the three-dot menu → **Settings and sharing**.
3. Scroll down to **Integrate calendar**.
4. Copy the **Calendar ID** (looks like `xxxxxxxxxx@group.calendar.google.com`).

Repeat for every calendar you want displayed.

### B6 — Fill in gcal_creds.rs

```rust
pub const CLIENT_ID:     &str = "NNNNNNNNNN-xxxxxxxxxxxxxxxxxxxxxxxx.apps.googleusercontent.com";
pub const CLIENT_SECRET: &str = "GOCSPX-xxxxxxxxxxxxxxxxxxxxxxxxxxxx";
pub const REFRESH_TOKEN: &str = "1//xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
pub const CALENDAR_IDS: &[&str] = &[
    "you@gmail.com",
    "xxxxxxxxxx@group.calendar.google.com",
    // add more as needed
];
```

---

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `(calendar offline)` in red after ~7 days | App still in *Testing* mode | Part A: publish the consent screen, re-run Step B4 |
| `invalid_grant` in server logs | Refresh token revoked or expired | Re-run Step B4 |
| `access_denied` during browser flow | Account not added as test user | Add your account in the OAuth consent screen Test Users list, or publish the app |
| Calendar shows but events are missing | Calendar ID not in `CALENDAR_IDS` | Add the missing calendar ID (Step B5) |
| Duplicate events | Same event appears in multiple calendars | Normal — `gcal.rs` deduplicates by summary + time |

---

## Token lifetime summary

| Consent screen status | Refresh token lifetime |
|---|---|
| Testing | **7 days** — expires even if used daily |
| In production (published) | **Indefinite** — expires only if unused for 6 months or manually revoked |
