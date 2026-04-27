# Teller.io Credentials Setup

Teller provides a free tier for personal use with no credit card required.
This is a one-time process. Once enrolled, credentials do not expire unless
you explicitly revoke them in the Teller dashboard.

Four values are needed:

| What | Where it goes |
|---|---|
| `access_token` | `server/src/teller_creds.rs` |
| `account_id` | `server/src/teller_creds.rs` |
| `certificate.pem` | server working directory |
| `private_key.pem` | server working directory |

---

## Step 1 — Create a Teller account and download your certificates

1. Go to <https://teller.io> and create a free account.
2. After logging in, go to **Applications → New Application**.
   Name it (e.g. `PhotoPainter`) and click **Create**.
3. Open the new application and click **Download Certificate**.
   This downloads a zip containing `certificate.pem` and `private_key.pem`.
4. Place both files in the directory you run the server from
   (the same directory as `stock_tickers.txt`).

The certificate identifies your application for all API calls via mTLS.
Keep `private_key.pem` secret — it never leaves your machine.

---

## Step 2 — Enroll your Bank of America account via Teller Connect

Teller Connect is a browser-based flow (similar to Plaid Link) that logs into
your bank and produces an `access_token`.

Save the following as `connect.html` anywhere on your machine.
Open the file in your application's dashboard to find your **Application ID**
(`app_xxxxxxxxxx`) and paste it where indicated.

```html
<!DOCTYPE html>
<html>
<head><meta charset="utf-8"><title>Teller Connect</title></head>
<body>
<button id="btn">Connect Bank of America</button>
<pre id="out" style="font-size:14px"></pre>
<script src="https://cdn.teller.io/connect/connect.js"></script>
<script>
  var handler = TellerConnect.setup({
    applicationId: "app_PASTE_YOUR_APP_ID_HERE",
    onSuccess: function(enrollment) {
      document.getElementById("out").textContent =
        "access_token: " + enrollment.accessToken + "\n" +
        "enrollment_id: " + enrollment.id + "\n\n" +
        "Copy access_token and proceed to Step 3.";
    },
    onExit: function() {
      document.getElementById("out").textContent = "Closed without completing.";
    }
  });
  document.getElementById("btn").onclick = function() { handler.open(); };
</script>
</body>
</html>
```

Open `connect.html` in a browser (`file:///path/to/connect.html`), click the button,
log in with your BofA credentials, and complete any MFA.
The page will display your `access_token`.

```rust
pub const ACCESS_TOKEN: &str = "token_...";
```

---

## Step 3 — Find your checking account ID

> **Windows note:** Windows curl uses schannel and cannot load PEM files directly.
> Use the Python snippet below instead of curl on Windows.

```python
import requests, json
resp = requests.get(
    'https://api.teller.io/accounts',
    cert=('certificate.pem', 'private_key.pem'),
    auth=('YOUR_ACCESS_TOKEN', ''))
print(json.dumps(resp.json(), indent=2))
```

Run with `python3 accounts.py` (install `requests` first if needed: `pip install requests`).

On Linux/Mac you can use curl instead:

```bash
curl -s --cert certificate.pem --key private_key.pem \
  -u "YOUR_ACCESS_TOKEN:" \
  https://api.teller.io/accounts | python3 -m json.tool
```

Look through the JSON for the entry with `"type": "depository"` and
`"subtype": "checking"`. Copy its `id` field.

```rust
pub const ACCOUNT_ID: &str = "acc_...";
```

---

## Step 4 — Verify the connection (optional)

Check that balance and transactions are reachable before starting the server.
Replace `TOKEN`, `CERT`, `KEY`, and `ACCOUNT_ID` with your values.

```python
import requests, json

TOKEN = "YOUR_ACCESS_TOKEN"
ACCT  = "YOUR_ACCOUNT_ID"
CERT  = ('certificate.pem', 'private_key.pem')

bal  = requests.get(f'https://api.teller.io/accounts/{ACCT}/balances',   cert=CERT, auth=(TOKEN,''))
txns = requests.get(f'https://api.teller.io/accounts/{ACCT}/transactions',cert=CERT, auth=(TOKEN,''))
print("Balance:", json.dumps(bal.json(),  indent=2))
print("Txns:",    json.dumps(txns.json(), indent=2))
```

Both should return JSON with numeric fields as quoted strings (e.g. `"available": "1234.56"`).

---

## Step 5 — Fill in teller_creds.rs and start the server

```rust
// server/src/teller_creds.rs
pub const ACCESS_TOKEN: &str = "token_...";
pub const ACCOUNT_ID:   &str = "acc_...";
pub const CERT_PATH:    &str = "teller_cert.pem";   // relative to server working dir
pub const KEY_PATH:     &str = "teller_key.pem";
```

Start the server in bank mode:

```bash
BANK_MODE=1 cargo run --release
```

The display shows the current available balance (black on yellow) followed by
the five most recent transactions (white on green).  A ` P` suffix marks pending
transactions.  Amounts prefixed with `-` are debits; `+` are credits.

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `could not read teller_cert.pem` in logs | Cert files not in working directory | Copy `certificate.pem` → `teller_cert.pem` and `private_key.pem` → `teller_key.pem` into the server's run directory |
| `could not parse mTLS identity` | Cert and key not concatenated correctly | The code concatenates them automatically — ensure both files are valid PEM |
| `(bank offline)` in red | API call failed | Run the Step 4 curl commands to diagnose; check `access_token` and `account_id` |
| HTTP 401 in logs | Invalid or revoked `access_token` | Re-run Step 2 to get a new token |
| Balance shown but wrong sign | Unlikely — Teller returns available balance as a positive number | Check the raw API response via curl |
| All amounts show `+` | All transactions are credits | Expected if account only received deposits recently |
