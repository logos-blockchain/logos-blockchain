#!/bin/sh

apt-get update && apt-get install -y --no-install-recommends inotify-tools

# Container can't write to the mounted host dir.
mkdir /usr/share/nginx/html/
cp -r /usr/share/nginx/html_template/* /usr/share/nginx/html/

envsubst '$ENV_TITLE_STRING' < /usr/share/nginx/html/index.html.template > /usr/share/nginx/html/index.html

mkdir -p /node-data/tracing
touch /node-data/tracing/otel_tokens.map

# If we update otlp auth tokens at runtime we need to reload the nginx service.
(
  while inotifywait -e modify /node-data/tracing/otel_tokens.map; do
      nginx -s reload
      echo "Token file modified, nginx gracefully reloaded."
  done
) &

exec nginx -g "daemon off;"
