# Jaspy snmptrapd-trapreader

## Usage

Example (minimal) snmptrapd.conf with default (deb) install

```
authCommunity log,execute,net somecommunityhere
traphandle IF-MIB::linkUp JASPY_URL=http://127.0.0.1:8000 /usr/lib/jaspy/jaspy-snmptrapd-reader
traphandle IF-MIB::linkDown JASPY_URL=http://127.0.0.1:8000 /usr/lib/jaspy/jaspy-snmptrapd-reader
```
