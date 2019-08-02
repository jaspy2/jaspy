import {DeployState} from "@/store/models";
<template>
    <div class="state-panel elevation-2">
        <div class="fqdn" title="Device FQDN"><span>{{ fqdn }}</span></div>
        <div class="spacer"></div>
        <div class="state" title="Deployment state">
            <v-select
                    label="Select State"
                    v-model="deployState"
                    :items="deployStates"
                    item-text="label"
                    item-value="value"
                    flat solo
                    hide-details></v-select>
        </div>
        <div class="transition" title="Transition to next state" @click="moveToNextState">
            <button>
                <v-icon>chevron_right</v-icon>
            </button>
        </div>
        <div class="configured" title="Configured" @click="toggleConfigured" :class="{ ok: isConfigured }">
            <button>
                <v-icon>settings</v-icon>
            </button>
        </div>
    </div>
</template>

<script lang="ts">
import { Component, Prop, Vue } from 'vue-property-decorator';
import { DeployState, Switch } from '@/store/models';

@Component
export default class SwitchStatePanel extends Vue {

    @Prop(String)
    public readonly fqdn!: string;
    @Prop(Number)
    public deployState!: number;
    @Prop(Boolean)
    public readonly isConfigured?: boolean;
    @Prop(Number)
    public readonly activePorts!: number;

    get state(): string {
        return this.deployStateToString(this.deployState);
    }

    @Prop(Function)
    public readonly updateItem!: (arg0: Switch) => Promise<void>;

    get deployStates(): Array<{label: string, value: number}> {
        const result = [] as Array<{label: string, value: number}>;
        for (const key in DeployState) {
            if (typeof(DeployState[key]) === 'number') {
                result.push({
                    label: this.deployStateToString(DeployState[key] as any as number),
                    value: DeployState[key] as any as number
                });
            }
        }
        return result;
    }

    public async moveToNextState() {
        let newState: DeployState;
        switch (this.deployState) {
            case DeployState.Stationed:
                newState = DeployState.InTransitToStorage;
                break;
            case DeployState.InTransitToStorage:
                newState = DeployState.InStorage;
                break;
            case DeployState.InStorage:
                newState = DeployState.InTransitToStation;
                break;
            case DeployState.InTransitToStation:
                newState = DeployState.Stationed;
                break;
            default:
                throw Error('Invalid deploy state during update.');
        }

        await this.updateItem({
            fqdn: this.fqdn,
            deployState: newState,
            configured: !!this.isConfigured
        });
    }

    public async toggleConfigured() {
        await this.updateItem({
            fqdn: this.fqdn,
            deployState: this.deployState,
            configured: !this.isConfigured
        });
    }

    private deployStateToString(state: DeployState): string {
        switch (state) {
            case DeployState.Stationed:
                return 'Stationed';
            case DeployState.InTransitToStorage:
                return 'To Storage';
            case DeployState.InStorage:
                return 'In Storage';
            case DeployState.InTransitToStation:
                return 'To Station';
            default:
                return 'Unknown';
        }
    }
}
</script>

<style scoped lang="scss">
.state-panel {
    display: grid;
    grid-template-columns: minmax(100px, 200px) auto 150px 70px 54px;

    background-color: #424242;
    color: rgba(255, 255, 255, 0.9);

    .fqdn {
        display: flex;
        margin: 0 15px;
        align-items: center;

        span {
            width: 100%;
            overflow: hidden;
            white-space: nowrap;
            text-overflow: ellipsis;
        }
    }

    .spacer {
        flex: 1;
    }

    .state {
        min-width: 100px;
    }

    .transition {
        padding-left: 15px;

        button {
            height: 48px;
            padding: 10px 15px;
            background-color: rgba(255, 255, 255, 0.1);
        }
    }

    .configured {
        button {
            height: 48px;
            padding: 10px 15px;
            background-color: rgba(255, 0, 0, 0.5);
        }

        &.ok button {
            background-color: rgba(0, 255, 0, 0.25);
        }
    }
}
</style>
