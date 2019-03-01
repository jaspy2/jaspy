#!/bin/bash
cd /opt/jaspy
diesel migration run
/usr/bin/jaspy-nexus
