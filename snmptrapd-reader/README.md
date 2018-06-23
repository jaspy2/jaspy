# Jaspy snmptrapd-trapreader

## Usage

Drop this to your snmptrapd.conf

```
traphandle IF-MIB::linkUp /path/to/trapreader/binary
traphandle IF-MIB::linkDown /path/to/trapreader/binary
```