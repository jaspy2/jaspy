import {getLinkColor,hostnameFromFQDN} from "./util.js";

export default class Link {
    constructor(remoteInterfaceName, sourceInterface) {
        this.remoteInterfaceName = remoteInterfaceName;
        this.graphicsObjectInfo = null;
        this.dirty = false;
        this.sourceInterface = sourceInterface;
        this.label = new PIXI.Text("<loading>", {fontFamily : '"Courier New", Courier, monospace', fontSize: 14, fill : 0xffffff, align : 'left'});

        //console.log("        create link -> " + remoteInterfaceName);
    }

    destroy() {
        //console.log("        destroy link -> " + this.remoteInterfaceName);
        this.sourceInterface.connectedToInterface = null;
        if(this.graphicsObjectInfo !== null) {
            this.graphicsObjectInfo["attachedTo"].removeChild(this.graphicsObjectInfo["object"]);
            this.graphicsObjectInfo = null;
        }
    }

    bindLabel(graphicsObject) {
        graphicsObject.link = this;
        graphicsObject.interactive = true;

        graphicsObject.mousemove = function(ev) {
            if(this.showLabel) {
                let curPoint = ev.data.global;
                curPoint.x += 20;
                let worldPoint = simulationGlobals.viewport.toWorld(curPoint);
                this.link.label.position = worldPoint;
                this.link.label.text = 
                    hostnameFromFQDN(this.link.sourceInterface.parentDeviceFQDN) + " " + this.link.sourceInterface.name +
                    " >> " +
                    hostnameFromFQDN(this.link.sourceInterface.connectedToInterface.parentDeviceFQDN) + " " + this.link.sourceInterface.connectedToInterface.name + "\n" +
                    this.link.getUsage().toFixed(2) + " Mbps";

                simulationGlobals.requestGraphicsUpdate = true;
            }
        }
        graphicsObject.mouseout = function(ev) {
            this.showLabel = false;
            simulationGlobals.viewport.removeChild(this.link.label);
            simulationGlobals.requestGraphicsUpdate = true;
        };
        graphicsObject.mouseover = function(ev) {
            this.showLabel = true;
            simulationGlobals.viewport.addChild(this.link.label);
        };
    }

    updateGraphics(viewport, startPosition, endPosition, widthOffset1, widthOffset2) {
        if(this.graphicsObjectInfo == null) {
            this.graphicsObjectInfo = {
                "object": new PIXI.Graphics(),
                "attachedTo": viewport
            };
            this.graphicsObjectInfo["attachedTo"].addChild(this.graphicsObjectInfo["object"]);
            this.bindLabel(this.graphicsObjectInfo["object"]);
        }

        let leftUpper = startPosition.clone().add(widthOffset1);
        let leftLower = startPosition.clone().add(widthOffset2);
        let rightUpper = endPosition.clone().add(widthOffset1);
        let rightLower = endPosition.clone().add(widthOffset2);

        let color = this.isUp() ? getLinkColor(this.getUtilization(), 1.0) : 0xff00ff;

        this.dirty = true;
        if(this.dirty) {
            let obj = this.graphicsObjectInfo["object"];
            obj.clear();
            obj.beginFill(color);
            obj.lineStyle(0,0x000000);
            obj.moveTo(leftUpper.x, leftUpper.y);
            obj.lineTo(leftLower.x, leftLower.y); obj.lineTo(rightLower.x, rightLower.y); obj.lineTo(rightUpper.x, rightUpper.y); obj.lineTo(leftUpper.x, leftUpper.y);
        }
    }

    isUp() {
        if(this.sourceInterface.status === null) {
            if(this.sourceInterface.connectedToInterface && this.sourceInterface.connectedToInterface.status !== null) {
                return this.sourceInterface.connectedToInterface.status;
            } else {
                return false;
            }
        } else {
            return this.sourceInterface.status;
        }
    }

    getSpeed() {
        if(this.sourceInterface) {
            if(this.sourceInterface.statisticsData) {
                return this.sourceInterface.statisticsData["speed_mbps"];
            } else {
                return 0;
            }
        } else {
            return 0;
        }
    }

    getUtilization() {
        if(this.sourceInterface) {
            if(this.sourceInterface.statisticsData && this.sourceInterface.statisticsData["tx_mbps"] != null) {
                return this.sourceInterface.statisticsData["tx_mbps"]/this.sourceInterface.statisticsData["speed_mbps"];
            } else if(this.sourceInterface.connectedToInterface && this.sourceInterface.connectedToInterface.statisticsData) {
                return this.sourceInterface.connectedToInterface.statisticsData["rx_mbps"]/this.sourceInterface.connectedToInterface.statisticsData["speed_mbps"];
            } else {
                return 0;
            }
        } else {
            return 0;
        }
    }

    getUsage() {
        if(this.sourceInterface) {
            if(this.sourceInterface.statisticsData && this.sourceInterface.statisticsData["tx_mbps"] != null) {
                return this.sourceInterface.statisticsData["tx_mbps"];
            } else if(this.sourceInterface.connectedToInterface && this.sourceInterface.connectedToInterface.statisticsData) {
                return this.sourceInterface.connectedToInterface.statisticsData["rx_mbps"];
            } else {
                return 0;
            }
        } else {
            return 0;
        }
    }
}