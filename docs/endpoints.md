# HTTP endpoints

`ingress`'s full HTTP surface (see [architecture.md](architecture.md#services-docker)). Every
request below is made to nginx on `127.0.0.1:3000`, which enforces the bearer token check before
proxying to `ingress`; `ingress` itself has no other inbound listener.

| Endpoint      | Method | Description                                                                                                    |
|---------------|--------|-------------------------------------------------------------------------------------------------------------------|
| `/`           | POST   | Enqueue one sentence for synthesis. Body: `{text, session, session_dir}`. Returns `202 {id}` on success, `400` if `session_dir` doesn't validate against `session`. |
| `/next`       | GET    | Query: `?session=<hash>`. Returns the next id due to play for that session and its status: `{id, filename, status}`, where `status` is `PROCESSING` or `COMPLETE`. `204` if nothing's pending. |
| `/ack`        | POST   | Body: `{id, session}`. Pops that id off the session's pending list once `player` has played and deleted it. |
| `/clear`      | POST   | Body: `{session}`. Drops everything queued (but not yet playing) for one session. |
| `/clear-all`  | POST   | No body. Drops everything queued for every known session, plus the shared work queue. |
