

export default class Interface {
    constructor(name) {
        this.name = name;
        this.topologyData = {};
        this.statisticsData = null;
        this.connectedToInterface = null;
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

    updateStatistics(data) {
        this.statisticsData = data;
    }
}