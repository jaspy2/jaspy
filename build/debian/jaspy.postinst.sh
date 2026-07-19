#!/bin/bash
id jaspy || useradd -Urm jaspy -s /bin/bash -d /var/lib/jaspy
su - postgres -c 'psql jaspy -c "SELECT 1"' || (
 su - postgres -c 'createuser jaspy'
 su - postgres -c 'createdb jaspy -O jaspy'
 su - postgres -c 'psql -t -c "REVOKE ALL ON DATABASE jaspy FROM PUBLIC"'
 su - postgres -c 'psql -t -c "GRANT ALL ON DATABASE jaspy TO jaspy"';
)
# No migration step here: jaspy-nexus applies its embedded migrations itself on
# startup (db::auto_migrate, on by default), so the deb ships neither the diesel
# CLI nor an on-disk migrations dir. The DB just needs to exist (above).
systemctl daemon-reload

