#!/bin/sh
set -e

if grep -Rq '${LLM_RESPONSE_TTS_BEARER_TOKEN}' /etc/nginx/conf.d/ 2>/dev/null; then
  echo "$0: LLM_RESPONSE_TTS_BEARER_TOKEN was not substituted into the nginx config (missing from docker/.env?). Refusing to start with an unauthenticated bearer-token check." >&2
  exit 1
fi

# Catches the case where substitution happened but the value was empty (e.g.
# LLM_RESPONSE_TTS_BEARER_TOKEN= in docker/.env) - that renders as
# `if ($http_authorization != "Bearer ")`, which anyone can satisfy by literally sending
# `Authorization: Bearer `.
if grep -Rq 'Bearer "' /etc/nginx/conf.d/ 2>/dev/null; then
  echo "$0: LLM_RESPONSE_TTS_BEARER_TOKEN substituted to an empty value. Refusing to start with a bypassable bearer-token check." >&2
  exit 1
fi
