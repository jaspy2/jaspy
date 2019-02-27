#!/bin/sh
set -e
DATABASE_URL=$JASPY_DB_URL diesel migration run
/usr/bin/jaspy-nexus
