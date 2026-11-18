Texture2D<uint4> cursor : register(t0);
Texture2D<float4> desktop : register(t1);

cbuffer Placement : register(b0) {
    int2 origin;
    int2 size;
    float2 offset;
    float2 scale;
};

float4 vs(uint id : SV_VertexID) : SV_Position {
    float2 corner = float2(id & 1, id >> 1);
    return float4(offset + corner * scale, 0, 1);
}

float4 ps(float4 position : SV_Position) : SV_Target {
    int2 pixel = int2(position.xy);
    uint4 shape = cursor.Load(int3(pixel - origin, 0));
    float3 background = desktop.Load(int3(pixel, 0)).rgb;

    if (shape.a > 255) {
        return float4((uint3(round(background * 255.0)) ^ shape.rgb) / 255.0, 1);
    }

    return float4(lerp(background, shape.rgb / 255.0, shape.a / 255.0), 1);
}
