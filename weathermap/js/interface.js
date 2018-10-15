

export default class Interface {
    constructor(name, parentDeviceFQDN) {
        this.parentDeviceFQDN = parentDeviceFQDN;
        this.name = name;
        this.topologyData = {};
        this.statisticsData = null;
        this.connectedToInterface = null;
        this.status = null;
        this.lastUpdate = 0;
        //console.log("    create interface " + name);
    }

    destroy() {
        //console.log("    destroy interface " + this.name);
    }

    updateTopologyData(data) {
        for(let [key, value] of Object.entries(data)) {
            this.topologyData[key] = value;
        }
    }

    setStatus(newStatus) {
        this.lastUpdate = ((new Date()).getTime()/1000.0);
        this.status = newStatus["state"];
    }

    updateStatistics(data) {
        this.lastUpdate = ((new Date()).getTime()/1000.0);
        this.statisticsData = data;
    }

    isStale() {
        let cur = ((new Date()).getTime()/1000.0);
        if(cur - this.lastUpdate > 60.0) {
            return true;
        } else {
            return false;
        }
    }
}