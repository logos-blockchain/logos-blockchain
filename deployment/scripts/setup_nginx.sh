#!/bin/sh

# Container can't write to the mounted host dir.
mkdir /usr/share/nginx/html/
cp -r /usr/share/nginx/html_template/* /usr/share/nginx/html/

envsubst '$ENV_TITLE_STRING' < /usr/share/nginx/html/index.html.template > /usr/share/nginx/html/index.html

exec nginx -g "daemon off;"
