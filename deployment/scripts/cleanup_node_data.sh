#!/bin/sh

rm -rf /node-data/*/state*
rm -rf /node-data/*/logos-blockchain.log.*
rm -rf /node-data/explorer

set -e

mkdir /node-data/explorer
cp /deployment.yaml /node-data/deployment.yaml
