#!/bin/sh

rm -rf /node-data/*/state*
rm -rf /node-data/*/logos-blockchain.log.*
rm -rf /node-data/cfgsync
rm -rf /node-data/explorer

set -e

mkdir /node-data/explorer
mkdir /node-data/cfgsync
cp /deployment.yaml /node-data/cfgsync/deployment-settings.yaml
