#!/bin/sh

ln -sf /node-data/explorer/sqlite.db /app/sqlite.db

exec python /app/src/main.py
