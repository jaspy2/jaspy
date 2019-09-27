#!/bin/bash
id snmpbot || useradd -Urm snmpbot -s /bin/bash -d /var/lib/snmpbot
systemctl daemon-reload
