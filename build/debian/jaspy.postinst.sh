#!/bin/bash
id jaspy || useradd -Urm jaspy -s /bin/bash -d /var/lib/jaspy
su - postgres -c 'psql jaspy -c "SELECT 1"' || (
 su - postgres -c 'createuser jaspy'
 su - postgres -c 'createdb jaspy -O jaspy'
 su - postgres -c 'psql -t -c "REVOKE ALL ON DATABASE jaspy FROM PUBLIC"'
 su - postgres -c 'psql -t -c "GRANT ALL ON DATABASE jaspy TO jaspy"';
)
su jaspy -c 'export DATABASE_URL=postgresql:///jaspy; cd /var/lib/jaspy/nexus && /usr/lib/jaspy/diesel migration run'
systemctl daemon-reload

