package dev.rime.bukkit;

import java.util.ArrayList;
import java.util.List;

import org.bukkit.Location;
import org.bukkit.Material;
import org.bukkit.World;
import org.bukkit.block.Block;
import org.bukkit.entity.Player;

final class RimeWorld implements World {
    static final RimeWorld INSTANCE = new RimeWorld();
    @Override public String getName() { return "world"; }
    @Override public Block getBlockAt(int x, int y, int z) { return new RimeBlock(x, y, z); }
    @Override public Block getBlockAt(Location l) { return getBlockAt(l.getBlockX(), l.getBlockY(), l.getBlockZ()); }
    @Override public long getTime() { return Math.floorMod(Bridge.worldTime(), 24000L); }
    @Override public void setTime(long time) { Bridge.setWorldTime(time); }
    @Override public long getFullTime() { return Bridge.worldTime(); }
    @Override public List<Player> getPlayers() { return new ArrayList<>(RimeServer.INSTANCE.getOnlinePlayers()); }
    @Override public Location getSpawnLocation() { return new Location(this, 0, 64, 0); }

    static final class RimeBlock implements Block {
        final int x, y, z;
        RimeBlock(int x, int y, int z) { this.x = x; this.y = y; this.z = z; }
        @Override public Material getType() {
            String n = Bridge.getBlock(x, y, z);
            Material m = n == null ? null : Material.matchMaterial(n);
            return m == null ? Material.AIR : m;
        }
        @Override public void setType(Material type) { Bridge.setBlock(x, y, z, type.name().toLowerCase(java.util.Locale.ROOT)); }
        @Override public int getX() { return x; }
        @Override public int getY() { return y; }
        @Override public int getZ() { return z; }
        @Override public World getWorld() { return INSTANCE; }
        @Override public Location getLocation() { return new Location(INSTANCE, x, y, z); }
        @Override public String toString() { return "Block{" + x + "," + y + "," + z + "}"; }
    }
}
