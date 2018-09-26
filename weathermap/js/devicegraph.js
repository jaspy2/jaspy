import Device from "./device.js";


export default class DeviceGraph {
    constructor(viewport) {
        this.viewport = viewport;
        this.devices = {}
        
        /*var sprite = this.viewport.addChild(new PIXI.Sprite(PIXI.Texture.WHITE));
        sprite.tint = 0xff0000;
        sprite.width = sprite.height = 100;
        sprite.position.set(100, 100);*/
    }

    updateTopologyData(data) {
        for(let [key, value] of Object.entries(data["devices"])) {
            let targetDevice = null;
            if(!(key in this.devices)) {
                this.devices[key] = new Device(key);
            }
            targetDevice = this.devices[key]
            targetDevice.updateTopologyData(value);
        }
        // Step 1, update devices
        // Step 2, update linkgroups { update link remote endpoints }
    }
}