#!/bin/sh

set -e

cd /etc/logos
exec /usr/bin/cfgsync-server /etc/logos/cfgsync.yaml
