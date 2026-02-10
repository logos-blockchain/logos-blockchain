# Systemd Service for Logos Blockchain Node

Template files for running the Logos Blockchain Node as a systemd service on bare metal / VPS.

## Setup

1. Copy and customize the service file:
   ```bash
   sudo cp logos-blockchain-node.service /etc/systemd/system/
   sudo nano /etc/systemd/system/logos-blockchain-node.service
   # Replace <username> with your actual username
   # Adjust paths to binary and config file
   ```

2. Reload systemd and start the service:
   ```bash
   sudo systemctl daemon-reload
   sudo systemctl start logos-blockchain-node
   # Optional: enable auto-start at boot
   sudo systemctl enable logos-blockchain-node
   ```

3. Check status and logs:
   ```bash
   sudo systemctl status logos-blockchain-node
   journalctl -u logos-blockchain-node -f
   ```

## Raspberry Pi / Volatile Storage

If journald logs are not persisting across reboots (common on Raspberry Pi), uncomment the `StandardOutput` and `StandardError` lines in the service file to write logs directly to a file.

When using file-based logging, optionally set up logrotate to prevent disk fill:
```bash
sudo cp logrotate-logos-blockchain-node.conf /etc/logrotate.d/logos-blockchain-node
sudo nano /etc/logrotate.d/logos-blockchain-node
# Replace <username> with your actual username
```

## Commands

```bash
# Start/stop/restart
sudo systemctl start logos-blockchain-node
sudo systemctl stop logos-blockchain-node
sudo systemctl restart logos-blockchain-node

# View logs (journald)
journalctl -u logos-blockchain-node -f           # follow
journalctl -u logos-blockchain-node --no-pager   # dump all

# Export logs to file
journalctl -u logos-blockchain-node --no-pager > node.log
```
