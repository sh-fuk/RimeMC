package dev.rime.bukkit;

import java.util.HashMap;
import java.util.UUID;

import org.bukkit.GameMode;
import org.bukkit.Location;
import org.bukkit.Server;
import org.bukkit.World;
import org.bukkit.entity.Player;
import org.bukkit.inventory.ItemStack;
import org.bukkit.inventory.PlayerInventory;

final class RimePlayer implements Player {
    private final String name;
    private final UUID uuid;
    private String displayName;

    RimePlayer(String name, UUID uuid) { this.name = name; this.uuid = uuid; this.displayName = name; }

    @Override public String getName() { return name; }
    @Override public UUID getUniqueId() { return uuid; }
    @Override public String getDisplayName() { return displayName; }
    @Override public void setDisplayName(String n) { displayName = n == null ? name : n; }
    @Override public void sendMessage(String message) { if (message != null) Bridge.sendMessage(name, message); }
    @Override public void sendMessage(String... messages) { for (String m : messages) sendMessage(m); }
    @Override public boolean isOp() { return Bridge.isOp(name); }
    @Override public boolean hasPermission(String permission) {
        if (isOp()) return true;
        Boolean d = Bridge.permissionDefaults.get(permission);
        return d != null && d;
    }
    @Override public Server getServer() { return RimeServer.INSTANCE; }
    @Override public boolean isOnline() { return Bridge.playerUuid(name) != null; }
    @Override public Location getLocation() {
        double[] p = Bridge.playerPos(name);
        if (p == null) return new Location(RimeWorld.INSTANCE, 0, 0, 0);
        return new Location(RimeWorld.INSTANCE, p[0], p[1], p[2], (float) p[3], (float) p[4]);
    }
    @Override public World getWorld() { return RimeWorld.INSTANCE; }
    @Override public boolean teleport(Location l) {
        Bridge.request(name, 0, null, l.getX(), l.getY(), l.getZ(), l.getYaw(), l.getPitch());
        return true;
    }
    @Override public GameMode getGameMode() { return GameMode.getByValue(Bridge.gamemode(name)); }
    @Override public void setGameMode(GameMode mode) { Bridge.request(name, 1, null, mode.getValue(), 0, 0, 0, 0); }
    @Override public void kickPlayer(String message) { Bridge.request(name, 2, message == null ? "Kicked" : message, 0, 0, 0, 0, 0); }
    @Override public boolean performCommand(String command) { Bridge.request(name, 3, command.startsWith("/") ? command.substring(1) : command, 0, 0, 0, 0, 0); return true; }
    @Override public void chat(String msg) { Bridge.request(name, 4, msg, 0, 0, 0, 0, 0); }
    @Override public double getHealth() { return Bridge.health(name); }
    @Override public double getMaxHealth() { return 20.0; }
    @Override public PlayerInventory getInventory() {
        return new PlayerInventory() {
            @Override public HashMap<Integer, ItemStack> addItem(ItemStack... items) {
                HashMap<Integer, ItemStack> left = new HashMap<>();
                for (int i = 0; i < items.length; i++) {
                    ItemStack s = items[i];
                    if (s == null) continue;
                    if (!Bridge.giveItem(name, s.getType().name().toLowerCase(java.util.Locale.ROOT), s.getAmount())) left.put(i, s);
                }
                return left;
            }
        };
    }
    @Override public int getEntityId() { return 0; }
    @Override public boolean equals(Object o) { return o instanceof Player && ((Player) o).getUniqueId().equals(uuid); }
    @Override public int hashCode() { return uuid.hashCode(); }
    @Override public String toString() { return "RimePlayer{name=" + name + "}"; }
}
