<template>
    <transition name="slide-fade">
        <div id="statusOverlay" class="status-overlay" v-if="visible">
            <v-progress-circular indeterminate size="64" color="green"></v-progress-circular>
        </div>
    </transition>
</template>

<script lang="ts">
import { Component, Prop, Vue, Watch } from 'vue-property-decorator';

@Component
export default class SwitchStatePanel extends Vue {

    @Prop(Boolean)
    public readonly visible!: boolean;

    @Watch('visible', { immediate: true })
    private visibilityChanged(newValue: boolean, oldValue: boolean): void {
        if (newValue) {
            document.documentElement.style.overflow = 'hidden';
        } else {
            document.documentElement.style.overflow = 'auto';
        }
    }

}
</script>

<style scoped lang="scss">
    #statusOverlay {
        position: absolute;
        top: 0;
        bottom: 0;
        left: 0;
        right: 0;
        display: flex;
        align-items: center;
        justify-content: center;
        overflow: hidden;
        max-height: 100vh;
        background-color: rgba(0, 0, 0, 0.5);
    }

    .slide-fade-enter-active {
        transition: all .3s ease;
    }
    .slide-fade-leave-active {
        transition: all .3s cubic-bezier(1.0, 0.5, 0.8, 1.0);
    }
    .slide-fade-enter, .slide-fade-leave-to
        /* .slide-fade-leave-active below version 2.1.8 */ {
        transform: translateX(10px);
        opacity: 0;
    }
</style>
