#!/bin/sh
cd /opt/jaspy
diesel --database-url $JASPY_DB_URL migration run
/usr/bin/jaspy-nexus
