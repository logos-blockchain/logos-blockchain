#!/bin/sh

envsubst '$ENV_TITLE_STRING' < /usr/share/nginx/html/index.html.template > /usr/share/nginx/html/index.html

exec nginx -g "daemon off;"
