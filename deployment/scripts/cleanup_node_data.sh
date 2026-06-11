#!/bin/sh

rm -rf /node-data/*/state*
rm -rf /node-data/*/logos-blockchain.log.*
rm -rf /node-data/cfgsync

set -e

mkdir /node-data/cfgsync
cp /deployment.yaml /node-data/cfgsync/deployment-settings.yaml
