#!/bin/sh
set -e

if grep -Rq '${LLM_RESPONSE_TTS_BEARER_TOKEN}' /etc/nginx/conf.d/ 2>/dev/null; then
  echo "$0: LLM_RESPONSE_TTS_BEARER_TOKEN was not substituted into the nginx config (missing from docker/.env?). Refusing to start with an unauthenticated bearer-token check." >&2
  exit 1
fi
