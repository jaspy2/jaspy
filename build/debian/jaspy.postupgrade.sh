su jaspy -c 'export DATABASE_URL=postgresql:///jaspy; cd /var/lib/jaspy/nexus && /usr/lib/jaspy/diesel migration run'
systemctl daemon-reload

