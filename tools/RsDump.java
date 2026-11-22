import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.core.BlockPos;
import net.minecraft.world.level.block.state.properties.NoteBlockInstrument;
import java.io.*;

public class RsDump {
    public static void main(String[] a) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        int n = Block.BLOCK_STATE_REGISTRY.size();
        byte[] out = new byte[n * 3];
        for (int i = 0; i < n; i++) {
            BlockState s = Block.BLOCK_STATE_REGISTRY.byId(i);
            int f = 0;
            if (s.isRedstoneConductor(EmptyBlockGetter.INSTANCE, BlockPos.ZERO)) f |= 1;
            if (s.isSignalSource()) f |= 2;
            if (s.hasAnalogOutputSignal()) f |= 4;
            if (s.hasBlockEntity()) f |= 8;
            if (s.getDestroySpeed(EmptyBlockGetter.INSTANCE, BlockPos.ZERO) == -1.0f) f |= 16;
            if (s.isSuffocating(EmptyBlockGetter.INSTANCE, BlockPos.ZERO)) f |= 32;
            if (s.canOcclude()) f |= 64;
            if (s.isRandomlyTicking()) f |= 128;
            out[i] = (byte) f;
            out[n + i] = (byte) s.getPistonPushReaction().ordinal();
            out[2 * n + i] = (byte) s.instrument().ordinal();
        }
        try (FileOutputStream o = new FileOutputStream(a[0])) { o.write(out); }
        StringBuilder b = new StringBuilder();
        for (NoteBlockInstrument x : NoteBlockInstrument.values()) b.append(x.getSerializedName()).append(' ').append(x.getSoundEvent().unwrapKey().map(k -> k.identifier().toString()).orElse("?")).append(' ').append(x.isTunable()).append(' ').append(x.worksAboveNoteBlock()).append('\n');
        try (PrintWriter p = new PrintWriter(a[1])) { p.print(b); }
        System.out.println("states " + n);
    }
}
