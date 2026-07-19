# jaspy-nexus applies its embedded migrations on startup, so an upgrade just
# needs to restart the service onto the new binary (which then migrates + serves).
# try-restart: restart only if it was already running; never surprise-start it.
systemctl daemon-reload
systemctl try-restart jaspy-nexus

