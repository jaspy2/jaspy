#!/bin/bash
# jaspy-switchmaster runs as the jaspy user (see jaspy-switchmaster.service).
# The jaspy "nexus" package normally creates that user; create it here too so
# this package installs cleanly on its own.
id jaspy || useradd -Urm jaspy -s /bin/bash -d /var/lib/jaspy
systemctl daemon-reload
