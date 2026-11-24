import { Chat } from "@/components/chat/chat";
import { DirectionalTransition } from "@/components/directional-transition";

export default function Page() {
  return (
    <DirectionalTransition>
      <Chat />
    </DirectionalTransition>
  );
}
