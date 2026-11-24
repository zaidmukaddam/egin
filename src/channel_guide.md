# Add a channel to an egin agent

You are adding a channel for {{provider}} to the egin agent in this folder. egin serves any platform that sends
signed JSON webhooks from a file, with no code: `channels/<name>.toml`, served at
`https://<server>/channels/<name>`.

## 1. Read the provider's webhook docs

{{source}}

Find, and write down with the URL you read each in:

- The header that carries the signature, the algorithm (HMAC-SHA256, HMAC-SHA1, or a shared token compared
  as is), the encoding (hex or base64), any prefix such as `sha256=`, and what is signed. egin signs the raw
  request body only: a provider that signs a timestamp plus the body, a URL, or form fields is out of reach
  for a described channel; say so and stop.
- The JSON pointer to the message text, and to what identifies one conversation (an issue, a ticket, a
  chat, a phone number).
- A delivery id, in the body or a header, if the provider retries deliveries.
- The fields that tell the events to answer (a new comment) from the rest, and how to recognise the
  agent's own messages so it does not answer itself.
- The API call that posts a reply into that conversation, its URL, auth header and JSON body.

Do not guess any of these. If the docs don't say, ask.

## 2. Write channels/<name>.toml

```toml
[verify]
header = "X-Signature"            # the signature header
secret = "${PROVIDER_WEBHOOK_SECRET}"
scheme = "hmac-sha256"            # hmac-sha256 (default), hmac-sha1, or token
encoding = "hex"                  # hex (default) or base64
prefix = ""                       # stripped from the header value first, such as "sha256="

[message]
text = "/data/body"               # JSON pointer to the text
thread = "/data/conversationId"   # one session per value; leave out for one session per request
id = "/deliveryId"                # or id_header = "X-Delivery-Id": a repeat delivery is skipped
only = { "/type" = "message" }    # answer only events where every pointer has this value
skip = { "/sender/id" = "${PROVIDER_BOT_ID}" }   # ignore events where any pointer has this value
# mention = "@agent"              # answer only text containing this; it is removed

[reply]                           # leave out, and the request waits and gets {"text": answer} back
url = "https://api.provider.com/conversations/{{thread}}/messages"
method = "POST"
headers = { Authorization = "Bearer ${PROVIDER_API_KEY}" }
body = '''{"text": {{answer}}}'''
```

`{{answer}}` and `{{thread}}` become JSON strings in `body` (quotes included, so don't add your own) and are
URL-encoded in `url`. `${VAR}` reads the server's environment, so the file holds no secret.

## 3. Check it

1. `egin info` lists the channel; a mistake in the file stops the agent from loading, with the reason.
2. Run `egin serve`, then send a signed request with a sample payload from the provider's docs:

```bash
body='<sample payload>'
sig=$(printf '%s' "$body" | openssl dgst -sha256 -hmac "$PROVIDER_WEBHOOK_SECRET" | sed 's/^.* //')
curl -s localhost:8787/channels/<name> -H "X-Signature: $sig" -H 'content-type: application/json' -d "$body"
```

A wrong signature must get `401`. A matching event starts a session (`GET /sessions` lists it) and the reply
goes to the URL in `[reply]`.

3. Tell the user which environment variables to set, and where in the provider's settings to point the
   webhook.
