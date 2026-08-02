# Available voices

Synthesis is done by [kokoros](https://github.com/lucasjinreal/kokoros), a Rust implementation of Kokoro
TTS.

## Configuring

Set `KOKOROS_VOICE` on the `worker` service in `docker-compose.yml` to any voice name below, then apply it:

```bash
docker compose up -d
```

No rebuild needed - it's an env var, not baked into the image.

## Available names

kokoros doesn't publish a complete voice catalog in its own README - the names it accepts come from the
underlying Kokoro-82M model's voicepack files. Known-working names used or referenced in this project:

- `af_heart` (default)
- `af_sky`, `af_bella`, `af_nicole`
- `bm_daniel`, `bm_george`

For the current full list, check the [kokoros repo](https://github.com/lucasjinreal/kokoros) directly.
