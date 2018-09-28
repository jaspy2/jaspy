

export default class Interface {
    constructor(name) {
        this.name = name;
        this.topologyData = {};
        this.statisticsData = null;
        this.connectedToInterface = null;
        this.status = null;
        console.log("    create interface " + name);
    }

    destroy() {
        console.log("    destroy interface " + this.name);
    }

    updateTopologyData(data) {
        for(let [key, value] of Object.entries(data)) {
            this.topologyData[key] = value;
        }
    }

    setStatus(newStatus) {
        this.status = newStatus["state"];
    }

    updateStatistics(data) {
        console.log(data);
        this.statisticsData = data;
    }
}