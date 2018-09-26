import Link from "./link.js";


export default class LinkGroup {
    constructor(name) {
        this.name = name;
        this.target = name;
        this.targetPosition = new Victor(0, 0);
        this.groupedLinks = {};
        console.log("        create linkgroup -> " + name);
    }

    destroy() {
        console.log("        destroy linkgroup -> " + this.name);
        for(let [key, value] of Object.entries(this.groupedLinks)) {
            this.groupedLinks[key].destroy();
            delete this.groupedLinks[key];
        }
    }

    updateTopologyData(linkgroupInfo) {
        console.log("    update linkgroup " + this.name);
        for(let [key, value] of Object.entries(linkgroupInfo)) {
            if(!(key in this.groupedLinks)) {
                this.groupedLinks[value["connectedTo"]["interface"]] = new Link(value["connectedTo"]["interface"]);
            }
        }
        for(let [key, value] of Object.entries(this.groupedLinks)) {
            if(!(key in linkgroupInfo)) {
                this.groupedLinks[key].destroy();
                delete this.groupedLinks[key];
            }
        }
    }

    updateTargetInfo(targetDevice) {
        this.targetPosition = targetDevice.getPosition();
    }
}