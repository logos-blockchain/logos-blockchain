#!/bin/sh

rm -rf /node-data/*/state*
rm -rf /node-data/*/logos-blockchain.log.*
rm -rf /node-data/explorer

set -e

mkdir /node-data/explorer
cp /deployment.yaml /node-data/deployment.yaml
# The era files the deployment includes, at the path it includes them by.
rm -rf /node-data/era
mkdir /node-data/era
cp -r /eras /node-data/era/schedule
