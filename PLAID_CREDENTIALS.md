# Plaid Credentials Setup

This is a one-time process. Once you have a valid `access_token` it does not expire.
All four values go into `server/src/plaid_creds.rs`.

---

## Step 1 — Create a Plaid developer account

1. Go to <https://dashboard.plaid.com/signup> and create a free account.
2. In the dashboard, go to **Team Settings → Keys**.
3. Copy your **client_id** and **Secret** (use the **Production** secret when ready; start with **Sandbox** for testing).

Fill these in now:
```rust
pub const CLIENT_ID: &str = "...";   // from dashboard
pub const SECRET:    &str = "...";   // Production secret
```

---

## Step 2 — Request Production access

Sandbox lets you test with fake data immediately.  
To connect to real Bank of America you need Production access.

1. In the dashboard go to **Overview → Request access to Production**.
2. Fill in the short form (personal project / own data / checking balance + transactions).
3. Approval is usually same-day for personal use.

You can complete Steps 3–5 in Sandbox first to verify everything works, then repeat with your Production secret.

---

## Step 3 — Create a Link token (curl)

Replace `CLIENT_ID` and `SECRET` below. For Sandbox use `https://sandbox.plaid.com`; for Production use `https://production.plaid.com`.

```bash
curl -s -X POST https://production.plaid.com/link/token/create \
  -H "Content-Type: application/json" \
  -d '{
    "client_id": "CLIENT_ID",
    "secret":    "SECRET",
    "client_name": "PhotoPainter",
    "country_codes": ["US"],
    "language": "en",
    "user": { "client_user_id": "local-user" },
    "products": ["transactions"]
  }' | python3 -m json.tool
```

Copy the `link_token` value from the response. It expires in 30 minutes.

---

## Step 4 — Complete the Link UI in a browser

Save the following as `link.html` anywhere on your machine, paste your `link_token` where indicated, then open the file in a browser (`file:///path/to/link.html`).

```html
<!DOCTYPE html>
<html>
<head><meta charset="utf-8"><title>Plaid Link</title></head>
<body>
<button id="btn">Connect Bank of America</button>
<pre id="out"></pre>
<script src="https://cdn.plaid.com/link/v2/stable/link-initialize.js"></script>
<script>
  const handler = Plaid.create({
    token: 'PASTE_LINK_TOKEN_HERE',
    onSuccess: function(public_token, metadata) {
      document.getElementById('out').textContent =
        'public_token:\n' + public_token + '\n\nCopy this value and run Step 5.';
    },
    onExit: function(err) {
      if (err) document.getElementById('out').textContent = JSON.stringify(err, null, 2);
    }
  });
  document.getElementById('btn').onclick = function() { handler.open(); };
</script>
</body>
</html>
```

Click **Connect Bank of America**, log in with your BofA credentials, and complete any MFA. The page will display your `public_token`.

---

## Step 5 — Exchange public_token for access_token (curl)

```bash
curl -s -X POST https://production.plaid.com/item/public_token/exchange \
  -H "Content-Type: application/json" \
  -d '{
    "client_id":    "CLIENT_ID",
    "secret":       "SECRET",
    "public_token": "PUBLIC_TOKEN_FROM_STEP_4"
  }' | python3 -m json.tool
```

Copy the `access_token` value (it starts with `access-production-...`).

```rust
pub const ACCESS_TOKEN: &str = "access-production-...";
```

---

## Step 6 — Find your checking account ID (curl)

```bash
curl -s -X POST https://production.plaid.com/accounts/get \
  -H "Content-Type: application/json" \
  -d '{
    "client_id":    "CLIENT_ID",
    "secret":       "SECRET",
    "access_token": "ACCESS_TOKEN"
  }' | python3 -m json.tool
```

Look through the `accounts` array for the entry with `"type": "depository"` and
`"subtype": "checking"`. Copy its `account_id`.

```rust
pub const ACCOUNT_ID: &str = "...";
```

---

## Step 7 — Start the server in bank mode

```bash
BANK_MODE=1 cargo run --release
```

The display will show the current balance (black on yellow) followed by the five most
recent transactions (white on green) above the Google Calendar entries.

---

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| `(bank offline)` in red | Fetch failed — check credentials and network |
| Balance shows but no transactions | Date range may not cover recent activity; the module looks back 30 days |
| Pending transactions shown with ` P` suffix | Normal — Plaid marks uncleared items as pending |
| `ITEM_LOGIN_REQUIRED` error in logs | BofA session expired — re-run Steps 3–5 to get a new access_token |
